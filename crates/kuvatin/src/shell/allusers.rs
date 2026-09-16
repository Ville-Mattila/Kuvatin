//! The `--unregister-all-users` entry point: one pass that takes Kuvatin's
//! Explorer menu, its Windows 11 menu package and its leftover files off
//! **every** account on the machine.
//!
//! The installer runs this as SYSTEM, from a deferred custom action, while the
//! exe it is running still exists. Three rules follow from that, and they are
//! the shape of this module:
//!
//! * It prints to **stdout** and nowhere else. `crate::applog` as SYSTEM would
//!   resolve `%LOCALAPPDATA%` to `C:\Windows\System32\config\systemprofile\…`
//!   and leave a brand-new folder behind — exactly the sort of leftover this
//!   mode exists to remove. Nothing here loads presets or settings either, for
//!   the same reason.
//! * Nothing here fails hard. The custom action is `Return='ignore'`, so the
//!   uninstall carries on whatever this says; one account whose hive is locked
//!   must not cost the other accounts theirs. What the run could not do is
//!   said, in the log, where somebody can read it and act on it.
//! * Nothing here waits for ever, either, and for the same reason: the accounts
//!   are visited one after another, so an account that could hold its own step
//!   open would hold up every account after it. Each one's file walk gets
//!   [`FILE_BUDGET`] and no more; the package sweep brings a deadline of its
//!   own; and the registry work is bounded by the hive it is walking.
//!
//! The work is gathered first and printed afterwards, and that is what makes it
//! testable: [`gather`] is all the machine-touching, and [`opening`] and
//! [`report`] are all the wording — pure functions over what the other modules
//! handed back. The tests build those structs by hand and read the lines, so
//! every rule about what gets printed (the order, the counts, the cap on a
//! hostile hive's hundred refusals, which cases are failures and which are not)
//! is pinned here rather than left to be proven by running a real uninstall.
//! Running a real one is the release workflow's job.
//!
//! The one thing not held back until the end is the opening: two lines saying
//! what this is and whether it has the token for it, printed and flushed before
//! any work starts, so an action that is killed or that hangs in the deployment
//! service still leaves a sign in the MSI log that it began.

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use super::files::{self, FileSweep};
use super::hive::{self, HiveAccess, HiveOutcome};
use super::package::{self, PackageReach, PackageSweep, PackageUser};
use super::paths;
use super::profiles::{self, Profile};
use super::verbs::VerbSweep;

/// The run did what it could. Refusals do not change this: the MSI action
/// ignores the code, so the code is for whoever reads the log, and what it says
/// is "this ran", not "this was perfect".
const EXIT_DONE: i32 = 0;

/// Nothing was attempted: without a full token neither another account's
/// classes hive nor its package registration can be opened.
const EXIT_NOT_ELEVATED: i32 = 1;

/// No account was cleaned: either none could be listed, or every one that was
/// had to be passed over. Which of the two is in the footer.
const EXIT_NO_ACCOUNTS: i32 = 2;

/// How many lines of one kind of trouble are printed before the rest are only
/// counted.
///
/// The capped lists are as long as somebody else cares to make them. A hive's
/// owner can plant a link under every one of a hundred `SystemFileAssociations`
/// children and have every key below each of them refused in the same breath,
/// and every name in their own `AppData\Local\Packages` is theirs to choose
/// too. Three and a count says as much as three hundred lines and leaves the
/// rest of the uninstall log readable.
const CAP: usize = 3;

/// How long one account's disk work may run before the uninstall stops waiting
/// for it and goes on to the next account.
///
/// There is a hole without it, and at both ends of that work. `remove_dir_all`
/// starts its enumeration again for every subdirectory it meets and caps
/// nothing, so an account that keeps creating directories under its own
/// `AppData\Local\Temp\kuvatin` while the sweep is running can keep it in that
/// loop for as long as it cares to. The listing that comes first is no safer:
/// `read_dir` keeps a resume position in the directory's index instead of taking
/// a snapshot, so names created during the scan that sort after the cursor come
/// back like any other, and an account looping on `Packages\zzz-0001`,
/// `zzz-0002`, … can keep it yielding just as long.
///
/// Nothing is gained by either — the directories are that account's own, and
/// SYSTEM reaches nowhere here it could not already reach — but this runs in a
/// deferred custom action, so one account could hold the whole machine's
/// uninstall open, and an uninstall that an unprivileged account can hang is
/// worth closing for its own sake.
///
/// It covers all of an account's disk work — the listing, the planning from it
/// and the walk — because the account owns every directory all three touch.
///
/// Sixty seconds because an ordinary account's file work is milliseconds: three
/// named files, one temp tree and one package data folder. A profile on a slow,
/// fragmented or network-backed disk might want a second or two; a thousand
/// times that is not a profile taking its time, it is a profile that will not
/// finish. The budget is per account, so a machine with N accounts could spend
/// N × 60 s in the worst case — only an account that reaches the budget spends
/// any of it, and a budget across the whole run is a possible follow-up if a
/// real machine ever shows one, exactly as `super::package`'s per-operation
/// deadline leaves its own total open.
const FILE_BUDGET: Duration = Duration::from_secs(60);

/// What every line of the report opens with, so these lines can be picked out
/// of an MSI log carrying everything else the uninstall did.
const PREFIX: &str = "Kuvatin: ";

/// The first line: what this mode is, and what it is about to do.
const HEADLINE: &str = "--unregister-all-users: taking the Explorer menu, the Windows 11 menu \
                        package and the leftover files off every account on this machine.";

/// Clean every account, print what happened, and hand back the exit code.
///
/// Thin on purpose: it says what it is about to do, it gathers, it words, it
/// prints. Everything worth testing is in the pure halves it calls.
pub fn unregister_all_users() -> i32 {
    let elevated = hive::is_elevated();
    print(&opening(elevated));
    let report = report(&gather(elevated));
    print(&report.lines);
    report.code
}

/// Put lines on stdout, and nowhere else, flushing as we go.
///
/// Flushed because the installer captures this through a pipe, where nothing is
/// line-buffered: unflushed, the opening lines would sit in a buffer until the
/// process ended, which is exactly the run that has none to show for itself.
///
/// A failed write is dropped on purpose. There is nothing to be done about a
/// stdout that will not take a line, and `println!` would panic on one — taking
/// a whole machine's cleanup down for the sake of a line of log, in a mode whose
/// custom action is `Return='ignore'` precisely so that nothing here can.
fn print(lines: &[String]) {
    let mut out = std::io::stdout().lock();
    for line in lines {
        let _ = writeln!(out, "{line}");
    }
    let _ = out.flush();
}

/// What one run gathered, before a word of it is printed.
///
/// Everything the report says comes from here, which is what lets the wording
/// be tested without a machine to clean.
#[derive(Debug, Default)]
struct Run {
    /// Whether this process holds a full token. Without one nothing below was
    /// attempted, and every field after this one is empty.
    elevated: bool,
    /// What removing the package for every account came to. `None` when the run
    /// stopped before it.
    package: Option<PackageSweep>,
    /// Why `SeBackupPrivilege` and `SeRestorePrivilege` could not be enabled,
    /// when they could not. The run carries on regardless: a signed-in
    /// account's hive is mounted already and needs neither privilege.
    privileges: Option<String>,
    /// What `profiles::all()` could not read, a line each as it came.
    trouble: Vec<String>,
    /// How many cleanable accounts it had to pass over. Counted by `profiles`
    /// from what it skipped, not read back out of the lines above, so a capped
    /// print can never change it.
    skipped: usize,
    /// One entry per account the run visited.
    accounts: Vec<Account>,
}

/// What visiting one account came to: its classes hive, then its files.
#[derive(Debug)]
struct Account {
    /// Its classes hive, and how that hive was reached.
    hive: HiveOutcome,
    /// Its disk work, when it finished inside [`FILE_BUDGET`].
    files: FileWork,
}

/// What one account's disk work came to, or why there is nothing to say about
/// it.
///
/// The listing's trouble lives inside `Done` rather than beside it, so that
/// there is no way to hold a reason from a listing that never finished: work
/// that ran out of time has nothing to report, not even about its first step.
#[derive(Debug)]
enum FileWork {
    /// It finished: what `paths::package_data_dirs` could not read under the
    /// account's `Packages` folder — each line already `<path>: <what
    /// happened>` — and what the walk over the plan then did.
    Done { dirs: Vec<String>, sweep: FileSweep },
    /// It was still going when [`FILE_BUDGET`] ran out. Nothing is counted from
    /// it: whatever it had removed by then belongs to work that had not
    /// finished, and counting half of it as a whole is how a machine gets called
    /// clean that is not.
    OutOfTime,
    /// It ended without answering, and this is what that looked like from here.
    Lost(String),
}

/// The report, ready to print, and what the process should exit with.
struct Report {
    lines: Vec<String>,
    code: i32,
}

/// Do the work, touching the machine and printing nothing.
///
/// The order is the plan's, and it matters: the package goes first, while
/// `kuvatin.exe` and `kuvatin_shellext.dll` are still on disk, because removing
/// the registration is what lets go of the DLL before `RemoveFiles` comes to
/// delete it.
fn gather(elevated: bool) -> Run {
    if !elevated {
        // Nothing below would work, and half of it would fail loudly enough to
        // read as a machine in trouble rather than as a shell without a token.
        return Run {
            elevated,
            ..Run::default()
        };
    }
    let package = Some(package::unregister_all_users());
    // Once for the whole run, before the first signed-out account needs it.
    let privileges = hive::enable_backup_restore().err();
    let (cleanable, trouble, skipped) = profiles::all();
    let accounts = cleanable.iter().map(clean).collect();
    Run {
        elevated,
        package,
        privileges,
        trouble,
        skipped: skipped.len(),
        accounts,
    }
}

/// Clean one account: its classes hive, then its files.
///
/// The hive first, and nothing at all between the two halves. For a signed-out
/// account `hive::clean_profile` mounts `UsrClass.dat`, which holds that file
/// exclusively until it unmounts again — which it does before it returns.
/// Putting the file walk in the middle would hold an account's hive open for
/// the length of a disk walk, for no gain whatever.
fn clean(profile: &Profile) -> Account {
    let hive = hive::clean_profile(profile);
    Account {
        hive,
        // `profile.dir` itself and not a path built to look like it: the walk
        // strips this exact path off the front of every planned path, component
        // by component, so it wants the value `profiles::vet_dir` vouched for.
        files: files_within_budget(profile.dir.clone()),
    }
}

/// Do all of one account's disk work — list its `Packages` folder, plan from
/// what is there, carry the plan out — and stop waiting for it after
/// [`FILE_BUDGET`].
///
/// All three inside the budget, because the account owns every directory all
/// three of them touch. The listing looks like the safe one and is not:
/// `read_dir` is `FindFirstFileW` and `FindNextFileW`, which keep a resume
/// position in the directory's index rather than taking a snapshot, so entries
/// created during the scan that sort after the cursor are handed back like any
/// other — and MSDN leaves what a concurrent change does undefined rather than
/// promising it ends. An account looping on `Packages\zzz-0001`, `zzz-0002`, …
/// can keep the listing yielding, and a budget that started after it would
/// never be reached.
///
/// On a thread of its own, with the wait on a channel, because that is the only
/// place the waiting can be bounded from. What does not end is inside a single
/// `read_dir` or `remove_dir_all` call, so a deadline checked between the plan's
/// entries would bound how many entries are attempted and not the one that never
/// returns. And nothing in [`super::files`] changes for it, which is the point:
/// that walk's argument about junctions, held handles and re-read attributes is
/// a delicate thing, and a deadline threaded through it would be a second reader
/// of every step for no gain here.
///
/// Work that runs out of time is left running rather than stopped. It cannot be
/// stopped — there is no way to interrupt a thread mid-syscall that is safe to
/// do to one holding open handles — and it does not need to be: what it holds
/// are handles inside that one account's profile, which it opened and vetted
/// itself. It either finishes on its own, unwatched, or it ends with this
/// process a moment later.
fn files_within_budget(profile: PathBuf) -> FileWork {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (found, dirs) = paths::package_data_dirs(&profile);
        let plan = paths::plan(&profile, &found);
        // A send nobody is waiting for any more is not an error worth having:
        // the budget ran out and the answer is late, which is what the caller
        // has already said.
        let _ = tx.send(FileWork::Done {
            dirs,
            sweep: files::remove_plan(&profile, &plan),
        });
    });
    match rx.recv_timeout(FILE_BUDGET) {
        Ok(done) => done,
        Err(mpsc::RecvTimeoutError::Timeout) => FileWork::OutOfTime,
        // The sender went without sending, which only a panic in the work does.
        Err(mpsc::RecvTimeoutError::Disconnected) => FileWork::Lost(
            "its file work ended without a word, which nothing but a panic does".to_string(),
        ),
    }
}

/// The two lines that go out before any work begins: what this mode is, and
/// whether it has the token it needs.
///
/// Pure, like the rest of the wording, so that the report's first two lines are
/// pinned by the same tests as everything after them even though they are
/// printed a few minutes earlier.
fn opening(elevated: bool) -> Vec<String> {
    let mut out = Lines::default();
    out.say(HEADLINE);
    out.say(format!(
        "this process is elevated: {}.",
        if elevated { "yes" } else { "no" }
    ));
    out.0
}

/// Everything after the opening: the lines a finished run prints, and the code
/// it exits with. Pure — it reads the structs and touches nothing.
fn report(run: &Run) -> Report {
    let mut out = Lines::default();
    // A run that never had a token attempted none of the steps below, so it
    // reports none of them: a step that did not run has nothing to say, and
    // saying it anyway reads as a step that found nothing.
    if run.elevated {
        if let Some(sweep) = &run.package {
            package_lines(sweep, &mut out);
        }
        if let Some(why) = &run.privileges {
            privilege_lines(why, &mut out);
        }
        account_list_lines(run, &mut out);
        for account in &run.accounts {
            account_lines(account, &mut out);
        }
    }
    let code = footer_lines(run, &mut out);
    Report { lines: out.0, code }
}

/// The package, for every account.
///
/// `Unsupported` is not a failure and must not read as one: a Windows that
/// predates the package cannot have one registered, so there is nothing to say
/// beyond which Windows this is — counts of a sweep that never ran would only
/// suggest a sweep that came up empty.
fn package_lines(sweep: &PackageSweep, out: &mut Lines) {
    out.say(format!(
        "the Windows 11 menu package, for every account: {}",
        sweep.reach.wording()
    ));
    if matches!(sweep.reach, PackageReach::Swept) {
        for found in &sweep.found {
            out.detail(format!(
                "found {}{}",
                found.full_name,
                registered_for(&found.users)
            ));
        }
        out.detail(format!(
            "{} registration(s) found, {} removed, {} still registered.",
            sweep.found.len(),
            sweep.removed,
            sweep.remaining.len()
        ));
    }
    // Uncapped, unlike the per-account lists: every line here is the deployment
    // service's own word about a real registration — one per account that still
    // has one, naming that account — so this list is as long as the machine has
    // accounts, not as long as somebody chose to make it. What is still
    // registered afterwards is in here too, a line per account, which is why
    // `remaining` is not printed again above.
    for line in &sweep.trouble {
        out.detail(line);
    }
}

/// Who a registration belongs to, as a clause after its name.
fn registered_for(users: &[PackageUser]) -> String {
    if users.is_empty() {
        return ", registered for no account the deployment service would name".to_string();
    }
    let who: Vec<String> = users
        .iter()
        .map(|user| format!("{} ({})", user.sid, user.state))
        .collect();
    format!(", registered for {}", who.join(", "))
}

/// The two privileges a signed-out account's hive needs, when they would not
/// enable. Worth a line of its own: the accounts that then keep their menu are
/// exactly the ones nobody is signed in to notice.
fn privilege_lines(why: &str, out: &mut Lines) {
    // The reason already says what it costs, so the frame only says what it is
    // about.
    out.say(format!("the backup and restore privileges: {why}"));
    out.detail("a signed-in account's hive is mounted already, so those are still cleaned.");
}

/// What the account list came to: everything it could not read, a line each,
/// and then how many accounts there are to clean and how many were passed over.
///
/// The trouble is printed with nothing in front of it: `profiles::all()` hands
/// back three kinds of line — the ProfileList key path, a sentence about a list
/// with nothing in it, and one `<SID>: <reason>` per account it passed over —
/// and no one prefix would suit all three.
///
/// An account passed over is an account that keeps its menu, so how many there
/// were is part of the result rather than a detail. The count comes from
/// `profiles` counting what it skipped, which is why capping the lines below
/// cannot quietly change it.
fn account_list_lines(run: &Run, out: &mut Lines) {
    if !run.trouble.is_empty() {
        out.say("the account list did not read in full:");
        out.capped(&run.trouble);
    }
    match run.accounts.len() {
        // Every machine this could be uninstalled from has an account on it, so
        // none at all is the shape of an enumeration that went wrong rather
        // than of a machine with nobody on it.
        0 => out.say(format!(
            "no account to clean, which is not what a machine Kuvatin was installed on looks like; \
             {} skipped.",
            run.skipped
        )),
        n => out.say(format!("{n} account(s) to clean; {} skipped.", run.skipped)),
    }
}

/// One account: how its hive was reached, what came off it, then its files.
fn account_lines(account: &Account, out: &mut Lines) {
    out.say(format!(
        "{}: {}",
        account.hive.sid,
        account.hive.access.wording()
    ));
    hive_lines(&account.hive, out);
    file_lines(account, out);
}

/// What came off one account's classes hive.
///
/// Every key is named inside that account's own hive and never
/// `HKCU\Software\Classes`, which on this path would name the hive of whoever
/// is running the uninstall — a different account, and the one hive this mode
/// is not about. A hive we mounted ourselves is named the same way: the
/// temporary mount is unmounted by the time this prints, so its name would
/// point at a key that no longer exists, while `HKEY_USERS\<SID>_Classes` is
/// where those keys are for that account and where somebody can go and look.
fn hive_lines(outcome: &HiveOutcome, out: &mut Lines) {
    let hive = format!(r"HKEY_USERS\{}_Classes", outcome.sid);
    verb_lines(&hive, &outcome.sweep, out);
    // The hive-level trouble is not about a key, so it takes no hive prefix;
    // what it needs is to be placed, and what places it is how far the hive was
    // reached at all.
    let framing = match outcome.access {
        HiveAccess::None => "why its hive was never reached",
        HiveAccess::Mounted => {
            "around mounting its hive (a line about unmounting means the hive is still mounted)"
        }
        HiveAccess::Loaded => "on the way to its hive",
    };
    for line in &outcome.trouble {
        out.detail(format!("{framing}: {line}"));
    }
}

/// The verb sweep: the counts first, then the one obstacle behind several
/// refusals if there is one, then everything it does not account for.
fn verb_lines(hive: &str, sweep: &VerbSweep, out: &mut Lines) {
    out.detail(format!(
        "{hive}: {} verb key(s) removed, {} already gone, {} refused.",
        sweep.removed, sweep.absent, sweep.refused
    ));
    // One planted link at `SystemFileAssociations` refuses every key below it,
    // in the same words each time. That is one thing to deal with, and it is
    // said once with its count rather than a dozen times.
    if let Some((why, count)) = sweep.shared_obstacle() {
        out.detail(format!(
            "{count} key(s) refused for one reason: {hive}\\{why}"
        ));
    }
    out.capped(&under(hive, sweep.other_refusals()));
    out.capped(&under(hive, sweep.troubles()));
    out.capped(&under(hive, sweep.notes()));
}

/// Name each of a sweep's lines inside the hive it came from. They all open
/// with a key relative to the classes root, so one prefix suits every kind.
fn under(hive: &str, lines: Vec<&str>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| format!("{hive}\\{line}"))
        .collect()
}

/// What came off one account's disk, or what became of the walk that was
/// taking it off.
fn file_lines(account: &Account, out: &mut Lines) {
    match &account.files {
        FileWork::Done { dirs, sweep } => {
            out.detail(format!(
                "files: {} removed, {} already gone; {} folder tree(s) removed, {} already gone; \
                 {} folder(s) pruned.",
                sweep.files_removed,
                sweep.files_absent,
                sweep.trees_removed,
                sweep.trees_absent,
                sweep.pruned
            ));
            // Both capped: a `Packages` folder holds names the account chose,
            // and the walk refuses each one it does not like by name.
            out.capped(dirs);
            out.capped(&sweep.trouble);
        }
        FileWork::OutOfTime => out.detail(format!(
            "files: still going after {} seconds, so the run stopped waiting and moved on to the \
             next account. What it had removed by then is not counted here; it either finishes on \
             its own or ends when this process does.",
            FILE_BUDGET.as_secs()
        )),
        FileWork::Lost(why) => out.detail(format!("files: {why}")),
    }
}

/// The last two lines: what the run added up to, and what it exits with.
///
/// Exit 0 whenever the run happened, refusals and all — the uninstall does not
/// stop for them, and the log is where the detail belongs. A non-zero code says
/// one thing only: the accounts were never reached. It is worth telling apart
/// from a run that reached them and was refused, because the answer to it is
/// different — run the uninstall as SYSTEM, or find out why the account list
/// would not read.
fn footer_lines(run: &Run, out: &mut Lines) -> i32 {
    if !run.elevated {
        out.say(
            "nothing was attempted: without a full token neither another account's classes hive \
             nor its package registration can be opened.",
        );
        out.say(format!(
            "exiting {EXIT_NOT_ELEVATED}: nothing was attempted, because this process is not \
             elevated."
        ));
        return EXIT_NOT_ELEVATED;
    }
    if run.accounts.is_empty() {
        // Not "nothing was attempted": the package sweep above ran and may well
        // have removed something. What did not happen is the accounts — and
        // there are two quite different ways for that, which must not be said
        // in the same words. A machine whose every cleanable account has a
        // junction where its profile directory should be listed its accounts
        // perfectly well; each of them was then passed over, and the reasons
        // are above.
        if run.skipped > 0 {
            out.say(format!(
                "no account could be cleaned: {} were listed and every one of them was passed \
                 over, for the reasons above; the package above is all that happened.",
                run.skipped
            ));
            out.say(format!(
                "exiting {EXIT_NO_ACCOUNTS}: every account that was listed had to be passed over."
            ));
        } else {
            out.say(
                "no account could be listed to clean, so no account's menu was touched; the \
                 package above is all that happened.",
            );
            out.say(format!(
                "exiting {EXIT_NO_ACCOUNTS}: no account could be listed to clean."
            ));
        }
        return EXIT_NO_ACCOUNTS;
    }
    out.say(format!(
        "done: {} account(s) visited; {} verb key(s) removed and {} refused; {} file(s) and \
         {} folder tree(s) removed, {} folder(s) pruned; {}{}.",
        run.accounts.len(),
        total(run, |account| account.hive.sweep.removed),
        total(run, |account| account.hive.sweep.refused),
        total(run, |account| swept(account).map_or(0, |f| f.files_removed)),
        total(run, |account| swept(account).map_or(0, |f| f.trees_removed)),
        total(run, |account| swept(account).map_or(0, |f| f.pruned)),
        package_total(run),
        worth_going_back_to(run)
    ));
    out.say(format!(
        "exiting {EXIT_DONE}: the run finished. A refusal above does not stop the uninstall. The \
         detail is in this log."
    ));
    EXIT_DONE
}

fn total(run: &Run, of: impl Fn(&Account) -> usize) -> usize {
    run.accounts.iter().map(of).sum()
}

/// One account's file counts, when there are any to have. A walk that ran out
/// of time or ended without a word has none, and adding nothing for it is the
/// whole point: the footer counts what was seen through, and that account is in
/// the tally of what to go back to instead.
fn swept(account: &Account) -> Option<&FileSweep> {
    match &account.files {
        FileWork::Done { sweep, .. } => Some(sweep),
        FileWork::OutOfTime | FileWork::Lost(_) => None,
    }
}

/// What is left to go back to, as a clause at the end of the footer: the
/// accounts with something to report, and the package when it had something of
/// its own. Empty when the run came up clean, which is the point — a footer
/// that always ended with a tally would say nothing by saying it every time.
fn worth_going_back_to(run: &Run) -> String {
    let accounts = run
        .accounts
        .iter()
        .filter(|account| account_reported(account))
        .count();
    let mut said = Vec::new();
    if accounts > 0 {
        said.push(format!("{accounts} account(s)"));
    }
    // The package's trouble is its own: a machine whose accounts all came clean
    // but whose registration would not go is not a clean machine.
    if run
        .package
        .as_ref()
        .is_some_and(|sweep| !sweep.trouble.is_empty())
    {
        said.push("the package".to_string());
    }
    if said.is_empty() {
        return String::new();
    }
    format!("; {} had something to report", said.join(" and "))
}

/// Whether one account is worth going back to: anything refused, anything that
/// would not read, anything that would not go — and a file walk that did not
/// finish, which is the plainest of them all.
fn account_reported(account: &Account) -> bool {
    !account.hive.trouble.is_empty()
        || account.hive.sweep.refused > 0
        || !account.hive.sweep.troubles().is_empty()
        || match &account.files {
            FileWork::Done { dirs, sweep } => !dirs.is_empty() || !sweep.trouble.is_empty(),
            FileWork::OutOfTime | FileWork::Lost(_) => true,
        }
}

/// The package's share of the footer, in the same words the section above used
/// — an unsupported Windows is still not a failure down here.
///
/// Clean means nothing to report, not an empty `remaining`. When the second
/// enumeration fails, `package.rs` has nothing to put in `remaining` and says so
/// in `trouble` instead, so a footer that counted `remaining` would call a
/// machine it could not read clean — the one case where the count is worth
/// least is the one where it looks best.
fn package_total(run: &Run) -> String {
    match run.package.as_ref().map(|sweep| (sweep.reach, sweep)) {
        Some((PackageReach::Swept, sweep)) if !sweep.trouble.is_empty() => format!(
            "the package removed for {} registration(s), and what is left is in the {} line(s) \
             above",
            sweep.removed,
            sweep.trouble.len()
        ),
        // Nothing to report and so nothing left: every registration that
        // outlived the removals put a line of its own in `trouble`.
        Some((PackageReach::Swept, sweep)) => format!(
            "the package removed for {} registration(s), with nothing left registered",
            sweep.removed
        ),
        Some((PackageReach::Unsupported, _)) => "no package on this Windows to remove".to_string(),
        Some((PackageReach::Unreachable, _)) | None => {
            "the package was not reached, so whatever is registered still is".to_string()
        }
    }
}

/// The lines as they are being written, each already carrying [`PREFIX`].
#[derive(Default)]
struct Lines(Vec<String>);

impl Lines {
    /// A line of the report's own.
    fn say(&mut self, text: impl AsRef<str>) {
        self.0.push(format!("{PREFIX}{}", text.as_ref()));
    }

    /// A line under the one before it — a detail of that step, or a reason from
    /// another module printed as it stands.
    fn detail(&mut self, text: impl AsRef<str>) {
        self.0.push(format!("{PREFIX}  {}", text.as_ref()));
    }

    /// At most [`CAP`] details, and then how many were left unsaid. See `CAP`
    /// for why these lists can run long.
    fn capped(&mut self, lines: &[impl AsRef<str>]) {
        for line in lines.iter().take(CAP) {
            self.detail(line);
        }
        if lines.len() > CAP {
            self.detail(format!("and {} more like it", lines.len() - CAP));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::package::Registration;
    use super::super::verbs::SweepLine;
    use super::*;

    const ALICE: &str = "S-1-5-21-1004336348-1177238915-682003330-1001";
    const BOB: &str = "S-1-5-21-1004336348-1177238915-682003330-1002";

    /// The whole report as the entry point prints it: the opening lines, which
    /// go out before the work starts, then everything `report` has to say about
    /// what the work came to. `unregister_all_users` does exactly this, with
    /// `gather` in between.
    fn full(run: &Run) -> Report {
        let mut lines = opening(run.elevated);
        let rest = report(run);
        lines.extend(rest.lines);
        Report {
            lines,
            code: rest.code,
        }
    }

    /// A run that went as well as a run can go: the package gone, two accounts
    /// cleaned, nothing refused.
    fn ordinary_run() -> Run {
        Run {
            elevated: true,
            package: Some(swept(1, 0)),
            privileges: None,
            trouble: Vec::new(),
            skipped: 0,
            accounts: vec![
                account(ALICE, HiveAccess::Loaded),
                account(BOB, HiveAccess::Mounted),
            ],
        }
    }

    fn swept(removed: usize, remaining: usize) -> PackageSweep {
        let full_name = "VilleMattila.Kuvatin_2.9.1.0_neutral__5jce0xfqz5w2a".to_string();
        let registration = Registration {
            full_name: full_name.clone(),
            users: vec![PackageUser {
                sid: ALICE.to_string(),
                state: "installed".to_string(),
            }],
        };
        PackageSweep {
            reach: PackageReach::Swept,
            found: vec![registration.clone()],
            removed,
            remaining: vec![registration; remaining],
            trouble: (0..remaining)
                .map(|_| format!("{full_name}: still registered for {ALICE} (staged)"))
                .collect(),
        }
    }

    fn account(sid: &str, access: HiveAccess) -> Account {
        Account {
            hive: HiveOutcome {
                sid: sid.to_string(),
                access,
                sweep: VerbSweep {
                    removed: 17,
                    absent: 0,
                    refused: 0,
                    lines: Vec::new(),
                },
                trouble: Vec::new(),
            },
            files: FileWork::Done {
                dirs: Vec::new(),
                sweep: FileSweep {
                    files_removed: 3,
                    files_absent: 0,
                    trees_removed: 2,
                    trees_absent: 0,
                    pruned: 1,
                    trouble: Vec::new(),
                },
            },
        }
    }

    /// One account's finished disk work, to be edited by a test that wants work
    /// which finished and had something to say.
    fn done_of(account: &mut Account) -> (&mut Vec<String>, &mut FileSweep) {
        match &mut account.files {
            FileWork::Done { dirs, sweep } => (dirs, sweep),
            other => panic!("this account's disk work did not finish: {other:?}"),
        }
    }

    /// Where a line saying `needle` is, or a failure quoting the whole report —
    /// an assertion about order is worth nothing if the line is simply not
    /// there.
    fn at(lines: &[String], needle: &str) -> usize {
        lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| {
                panic!(
                    "no line contains {needle:?}; the report was:\n{}",
                    lines.join("\n")
                )
            })
    }

    fn matching<'a>(lines: &'a [String], needle: &str) -> Vec<&'a str> {
        lines
            .iter()
            .filter(|line| line.contains(needle))
            .map(String::as_str)
            .collect()
    }

    /// The order is the plan's: what this is, the package (first, while the exe
    /// is still on disk), the accounts, each account's hive before its files,
    /// then the footer.
    #[test]
    fn the_report_reads_in_the_order_the_run_happened() {
        let report = full(&ordinary_run());
        let l = &report.lines;
        // The account's own heading line, and not merely its SID: the package
        // section above names the same account, which is exactly right there
        // and would make a search for the bare SID find the wrong line.
        let alice = format!("{PREFIX}{ALICE}: ");
        let bob = format!("{PREFIX}{BOB}: ");

        assert_eq!(
            at(l, "--unregister-all-users"),
            0,
            "the headline comes first"
        );
        assert!(at(l, "elevated") < at(l, "menu package, for every account"));
        assert!(at(l, "menu package, for every account") < at(l, "account(s) to clean"));
        assert!(at(l, "account(s) to clean") < at(l, &alice));
        assert!(
            at(l, &alice) < at(l, &bob),
            "accounts in the order they were visited"
        );
        // Within one account: the hive, then the files.
        let alice_files = l
            .iter()
            .enumerate()
            .skip(at(l, &alice))
            .find(|(_, line)| line.contains("folder(s) pruned"))
            .expect("alice's files")
            .0;
        assert!(at(l, "verb key(s)") < alice_files);
        assert!(alice_files < at(l, &bob));
        assert_eq!(*l.last().expect("a footer"), l[at(l, "exiting")]);
        assert_eq!(report.code, 0);
    }

    /// Every line is a Kuvatin line, so an MSI log full of everything else the
    /// uninstall did can still be read for what this mode said.
    #[test]
    fn every_line_says_whose_it_is() {
        let lines = full(&ordinary_run()).lines;
        assert!(lines.len() > 8, "a thin report proves nothing: {lines:#?}");
        for line in &lines {
            assert!(line.starts_with("Kuvatin: "), "stray line: {line}");
        }
    }

    /// Nothing on this path may name `HKCU`: the whole point of the mode is
    /// that it is cleaning somebody else's hive, and a line saying `HKCU` would
    /// be describing the hive of whoever happens to be running the uninstall.
    #[test]
    fn a_hive_line_names_the_accounts_own_hive() {
        let mut run = ordinary_run();
        run.accounts[0].hive.sweep = VerbSweep {
            removed: 5,
            absent: 0,
            refused: 1,
            lines: vec![SweepLine::Refused(
                r"Directory\shell\Kuvatin: we are not allowed to delete it (status 0xc0000022)"
                    .to_string(),
            )],
        };
        let lines = full(&run).lines;

        let refusals = matching(&lines, r"Directory\shell\Kuvatin");
        assert_eq!(refusals.len(), 1, "{lines:#?}");
        assert!(
            refusals[0].contains(&format!(r"HKEY_USERS\{ALICE}_Classes\Directory")),
            "a refusal must name the account's own hive: {refusals:?}"
        );
        assert!(
            matching(&lines, "HKCU").is_empty(),
            "no line may name the uninstalling account's hive"
        );
    }

    /// An account that could not be listed is an account that keeps its menu,
    /// so the count of them is part of the result, not a detail.
    #[test]
    fn the_count_says_how_many_accounts_were_skipped() {
        let mut run = ordinary_run();
        run.accounts.truncate(1);
        run.trouble = vec![
            // A reason with a semicolon of its own, which is what `profiles`
            // hands back one line at a time so that nothing here has to guess
            // where one reason ends and the next begins.
            "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\ProfileList: we are not allowed to \
             open it (error 5); read 6 before it"
                .to_string(),
            format!("{BOB}: C:\\Users\\bob does not exist"),
        ];
        run.skipped = 1;
        let lines = full(&run).lines;

        assert_eq!(
            matching(&lines, "1 account(s) to clean").len(),
            1,
            "{lines:#?}"
        );
        assert_eq!(matching(&lines, "1 skipped").len(), 1, "{lines:#?}");
        // One line each, printed as they came, with no hive in front of them
        // and the semicolon inside the first one left whole.
        assert_eq!(
            matching(&lines, "ProfileList: we are not allowed").len(),
            1,
            "{lines:#?}"
        );
        assert!(
            matching(&lines, "read 6 before it")[0].contains("error 5"),
            "a reason must not be cut in half: {lines:#?}"
        );
        assert_eq!(matching(&lines, "C:\\Users\\bob does not exist").len(), 1);
    }

    /// A machine Kuvatin was installed on has an account on it. None at all is
    /// the shape of a broken enumeration, and must not read as a clean sweep.
    #[test]
    fn no_account_at_all_is_suspicious_and_not_a_success() {
        let mut run = ordinary_run();
        run.accounts.clear();
        run.trouble = vec![
            "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\ProfileList listed no accounts at \
             all, which no Windows machine does"
                .to_string(),
        ];
        let report = full(&run);

        assert_eq!(report.code, 2, "{:#?}", report.lines);
        assert!(!matching(&report.lines, "no account to clean").is_empty());
        assert!(
            !matching(&report.lines, "no account could be listed to clean").is_empty(),
            "the footer must say why: {:#?}",
            report.lines
        );
        // The package sweep above did run, so the footer must not call the
        // whole run a thing that never happened.
        assert!(
            matching(&report.lines, "nothing was attempted").is_empty(),
            "the package was attempted: {:#?}",
            report.lines
        );
    }

    /// One account whose hive refused `count` keys, each in its own words so
    /// that no two of them share an obstacle.
    fn run_refusing(count: usize) -> Run {
        let mut run = ordinary_run();
        run.accounts.truncate(1);
        run.accounts[0].hive.sweep = VerbSweep {
            removed: 0,
            absent: 0,
            refused: count,
            lines: (0..count)
                .map(|i| {
                    SweepLine::Refused(format!(
                        r"SystemFileAssociations\.x{i:03}\shell\Kuvatin: we are not allowed to delete it (status 0xc0000022)"
                    ))
                })
                .collect(),
        };
        run
    }

    /// A hostile hive can refuse hundreds of keys in a hundred different ways.
    /// The log says three and counts the rest.
    #[test]
    fn a_long_list_of_refusals_is_capped() {
        let lines = full(&run_refusing(50)).lines;

        assert_eq!(
            matching(&lines, "SystemFileAssociations").len(),
            3,
            "three refusals and no more: {lines:#?}"
        );
        assert_eq!(matching(&lines, "and 47 more").len(), 1, "{lines:#?}");
        // The count is still the whole truth, capped list or not.
        assert!(!matching(&lines, "50 refused").is_empty(), "{lines:#?}");
    }

    /// The cap speaks only when it has left something out. A list of exactly
    /// three is a whole list, and "and 0 more like it" under one would be a
    /// line that says nothing about nothing.
    #[test]
    fn the_cap_stays_quiet_when_it_has_left_nothing_out() {
        let whole = full(&run_refusing(CAP)).lines;
        assert_eq!(matching(&whole, r"shell\Kuvatin").len(), CAP, "{whole:#?}");
        assert!(matching(&whole, "more like it").is_empty(), "{whole:#?}");

        let one_over = full(&run_refusing(CAP + 1)).lines;
        assert_eq!(
            matching(&one_over, r"shell\Kuvatin").len(),
            CAP,
            "{one_over:#?}"
        );
        assert_eq!(
            matching(&one_over, "and 1 more like it").len(),
            1,
            "{one_over:#?}"
        );
    }

    /// A candidate refused when it was read and again when it was deleted is
    /// two lines about one key; a note the deletion left along the way is a
    /// third kind of line; and every one of them is named inside the account's
    /// own hive.
    #[test]
    fn every_kind_of_sweep_line_is_named_inside_the_accounts_hive() {
        let mut run = ordinary_run();
        run.accounts.truncate(1);
        let key = r"SystemFileAssociations\.qoi\shell\Kuvatin";
        run.accounts[0].hive.sweep = VerbSweep {
            removed: 16,
            absent: 0,
            refused: 1,
            lines: vec![
                SweepLine::Trouble(format!("{key}: we are not allowed to open it (error 5)")),
                SweepLine::Note(
                    r"Directory\shell\Kuvatin: took a REG_LINK's entry without following it"
                        .to_string(),
                ),
                SweepLine::Refused(format!(
                    "{key}: we are not allowed to delete it (status 0xc0000022)"
                )),
            ],
        };
        // A hive that was reached and then had something to say about itself.
        run.accounts[0].hive.trouble = vec![format!(
            r"HKEY_USERS\{ALICE}_Classes: we are not allowed to open it (error 5)"
        )];
        let lines = full(&run).lines;
        let hive = format!(r"HKEY_USERS\{ALICE}_Classes\");

        let about_the_key = matching(&lines, ".qoi");
        assert_eq!(about_the_key.len(), 2, "one key, two lines: {lines:#?}");
        assert!(about_the_key
            .iter()
            .any(|line| line.contains("not allowed to open")));
        assert!(about_the_key
            .iter()
            .any(|line| line.contains("not allowed to delete")));
        for line in &about_the_key {
            assert!(line.contains(&hive), "not named in its hive: {line}");
        }
        let note = matching(&lines, "without following it");
        assert_eq!(note.len(), 1, "{lines:#?}");
        assert!(note[0].contains(&hive), "{}", note[0]);
        // A hive that was already mounted was reached, whatever else went on.
        assert!(
            !matching(&lines, "on the way to its hive").is_empty(),
            "{lines:#?}"
        );
    }

    /// Twelve keys refused because of one planted link is one thing to deal
    /// with, and it is said once, with its count, rather than twelve times.
    #[test]
    fn one_obstacle_behind_many_refusals_is_named_once() {
        let mut run = ordinary_run();
        run.accounts.truncate(1);
        let shared =
            r"SystemFileAssociations: carries a REG_LINK SymbolicLinkValue; not walking through it";
        run.accounts[0].hive.sweep = VerbSweep {
            removed: 5,
            absent: 0,
            refused: 13,
            lines: (0..12)
                .map(|_| SweepLine::Refused(shared.to_string()))
                .chain(std::iter::once(SweepLine::Refused(
                    r"image\shell\Kuvatin: we are not allowed to delete it (status 0xc0000022)"
                        .to_string(),
                )))
                .collect(),
        };
        let lines = full(&run).lines;

        assert_eq!(
            matching(&lines, "carries a REG_LINK").len(),
            1,
            "the one obstacle is said once: {lines:#?}"
        );
        assert!(!matching(&lines, "12 key(s)").is_empty(), "{lines:#?}");
        // …and the refusal that had nothing to do with it is still printed.
        assert_eq!(
            matching(&lines, r"image\shell\Kuvatin").len(),
            1,
            "{lines:#?}"
        );
    }

    /// A Windows that predates the package has nothing of the sort on it. That
    /// is an ordinary machine, not a failure, and the run still exits 0.
    #[test]
    fn a_windows_without_the_package_is_not_a_failure() {
        let mut run = ordinary_run();
        run.package = Some(PackageSweep {
            reach: PackageReach::Unsupported,
            found: Vec::new(),
            removed: 0,
            remaining: Vec::new(),
            trouble: Vec::new(),
        });
        let report = full(&run);

        assert_eq!(report.code, 0);
        assert_eq!(
            matching(&report.lines, "menu package, for every account").len(),
            1,
            "nothing to say beyond which Windows this is: {:#?}",
            report.lines
        );
        assert!(!matching(&report.lines, "predates the package").is_empty());
        assert!(
            matching(&report.lines, "registration(s) found").is_empty(),
            "counts of a sweep that never ran are noise: {:#?}",
            report.lines
        );
        assert!(
            !matching(&report.lines, "no package on this Windows").is_empty(),
            "and the footer says the same: {:#?}",
            report.lines
        );
    }

    /// The deployment service refusing to answer is a different matter: that
    /// one is trouble and says so, though it still does not stop the uninstall.
    #[test]
    fn a_deployment_service_that_would_not_answer_is_trouble() {
        let mut run = ordinary_run();
        run.package = Some(PackageSweep {
            reach: PackageReach::Unreachable,
            found: Vec::new(),
            removed: 0,
            remaining: Vec::new(),
            trouble: vec!["the deployment service would not start (0x80070422)".to_string()],
        });
        let report = full(&run);

        assert_eq!(report.code, 0, "the accounts were still cleaned");
        assert!(!matching(&report.lines, "would not answer").is_empty());
        assert!(
            !matching(&report.lines, "0x80070422").is_empty(),
            "printed as it came: {:#?}",
            report.lines
        );
    }

    /// Refusals do not change the exit code: the MSI action ignores it, the log
    /// carries the detail, and an uninstall that stopped here would leave the
    /// user with a half-removed program.
    #[test]
    fn refusals_do_not_make_the_run_a_failure() {
        let mut run = ordinary_run();
        run.accounts[0].hive.sweep.refused = 4;
        done_of(&mut run.accounts[0]).1.trouble = vec![
            r"C:\Users\alice\AppData\Local\Kuvatin\kuvatin.log: we are not allowed to delete it (os error 5)"
                .to_string(),
        ];
        run.accounts[1].hive.trouble = vec![
            r"HKEY_USERS\S-1-5-21-x_Classes: we are not allowed to open it (error 5)".to_string(),
        ];
        let report = full(&run);

        assert_eq!(report.code, 0, "{:#?}", report.lines);
        // Printed as it came: a path and a reason, not reworded.
        assert!(!matching(&report.lines, "kuvatin.log: we are not allowed").is_empty());
        // The hive-level trouble is placed by how far the hive was reached.
        assert!(
            !matching(&report.lines, "around mounting its hive").is_empty(),
            "{:#?}",
            report.lines
        );
        assert!(
            !matching(&report.lines, "had something to report").is_empty(),
            "the footer counts the accounts worth going back to: {:#?}",
            report.lines
        );
    }

    /// A hive that was never reached says so in those words, and not in the
    /// words of one that was mounted.
    #[test]
    fn a_hive_that_was_never_reached_is_worded_as_such() {
        let mut run = ordinary_run();
        run.accounts.truncate(1);
        run.accounts[0].hive.access = HiveAccess::None;
        run.accounts[0].hive.sweep = VerbSweep::default();
        run.accounts[0].hive.trouble =
            vec![r"HKEY_USERS\S-1-5-21-x_Classes: no such key".to_string()];
        let lines = full(&run).lines;

        assert!(!matching(&lines, "never reached").is_empty(), "{lines:#?}");
        assert!(matching(&lines, "around mounting").is_empty(), "{lines:#?}");
    }

    /// Without a full token there is no other account's hive to open and no
    /// package to remove for anybody, so nothing is attempted at all and the
    /// code says so.
    #[test]
    fn a_run_without_a_full_token_attempts_nothing() {
        let report = full(&Run::default());

        assert_eq!(report.code, 1, "{:#?}", report.lines);
        assert!(!matching(&report.lines, "elevated: no").is_empty());
        assert!(!matching(&report.lines, "nothing was attempted").is_empty());
        assert!(
            matching(&report.lines, "menu package, for every account").is_empty(),
            "a step that never ran must not be reported: {:#?}",
            report.lines
        );
    }

    /// The privileges are enabled once for the run, and a failure to enable
    /// them is worth a line: signed-out accounts are the ones that then keep
    /// their menu.
    #[test]
    fn privileges_that_would_not_enable_are_said_once() {
        let mut run = ordinary_run();
        run.privileges = Some(
            "this process's token does not hold SeBackupPrivilege, so a signed-out account's hive \
             cannot be mounted"
                .to_string(),
        );
        let report = full(&run);

        assert_eq!(report.code, 0);
        assert_eq!(matching(&report.lines, "SeBackupPrivilege").len(), 1);
        assert!(at(&report.lines, "SeBackupPrivilege") < at(&report.lines, "account(s) to clean"));
    }

    /// Package data folders live under a name the account chooses, so an
    /// unreadable `Packages` folder is reported per account, in its place.
    #[test]
    fn an_unreadable_packages_folder_is_reported_for_that_account() {
        let mut run = ordinary_run();
        *done_of(&mut run.accounts[0]).0 = vec![
            r"C:\Users\alice\AppData\Local\Packages would not be read (os error 5)".to_string(),
        ];
        let lines = full(&run).lines;

        let where_said = at(&lines, "Packages would not be read");
        assert!(at(&lines, &format!("{PREFIX}{ALICE}: ")) < where_said);
        assert!(
            where_said < at(&lines, &format!("{PREFIX}{BOB}: ")),
            "under the account it belongs to"
        );
    }

    /// The footer adds up what the run did, so a reader who skips the middle
    /// still knows whether the machine came clean.
    #[test]
    fn the_footer_adds_the_run_up() {
        let lines = full(&ordinary_run()).lines;
        let footer = lines[at(&lines, "done:")].to_string();

        assert!(footer.contains("2 account(s)"), "{footer}");
        assert!(footer.contains("34 verb key(s)"), "{footer}");
        assert!(footer.contains("6 file(s)"), "{footer}");
        assert!(footer.contains("4 folder tree(s)"), "{footer}");
        assert!(footer.contains("2 folder(s) pruned"), "{footer}");
        assert!(footer.contains("1 registration(s)"), "{footer}");
        // Nothing to report is what "clean" means, and a clean run says so
        // without a tally of who had something to say.
        assert!(footer.contains("nothing left registered"), "{footer}");
        assert!(!footer.contains("had something to report"), "{footer}");
    }

    /// An account can hold its own file walk open for as long as it likes —
    /// `remove_dir_all` starts again at every subdirectory it meets — so the
    /// run stops waiting, says so under that account, counts nothing from a
    /// walk that has not finished, and goes on to the next account.
    #[test]
    fn an_account_whose_files_ran_out_of_time_is_said_and_counted() {
        let mut run = ordinary_run();
        run.accounts[0].files = FileWork::OutOfTime;
        let report = full(&run);
        let lines = &report.lines;
        let said = at(lines, "still going after");

        assert!(
            lines[said].contains("60 seconds"),
            "the budget must be in the line: {}",
            lines[said]
        );
        // Under the account it belongs to, and not the one after it.
        assert!(
            at(lines, &format!("{PREFIX}{ALICE}: ")) < said,
            "{lines:#?}"
        );
        assert!(said < at(lines, &format!("{PREFIX}{BOB}: ")), "{lines:#?}");

        // The next account was visited all the same, and the run finished.
        assert_eq!(report.code, 0, "{lines:#?}");
        let footer = lines[at(lines, "done:")].to_string();
        assert!(footer.contains("2 account(s) visited"), "{footer}");
        assert!(
            footer.contains("1 account(s) had something to report"),
            "{footer}"
        );
        // Nothing is counted from the walk that did not finish: the totals are
        // the other account's alone.
        assert!(
            footer.contains("3 file(s) and 2 folder tree(s) removed, 1 folder(s) pruned"),
            "{footer}"
        );
    }

    /// A machine whose every cleanable account has a junction where its profile
    /// directory should be listed its accounts perfectly well and then passed
    /// over every one of them. Saying none could be listed would send whoever
    /// reads it to the wrong end of the problem.
    #[test]
    fn accounts_that_were_all_passed_over_are_not_an_empty_list() {
        let mut run = ordinary_run();
        run.accounts.clear();
        run.skipped = 3;
        run.trouble = (1..=3)
            .map(|n| format!("S-1-5-21-1-2-3-100{n}: C:\\Users\\u{n} is a reparse point"))
            .collect();
        let report = full(&run);

        assert_eq!(report.code, 2, "{:#?}", report.lines);
        assert!(
            !matching(
                &report.lines,
                "3 were listed and every one of them was passed over"
            )
            .is_empty(),
            "{:#?}",
            report.lines
        );
        assert!(
            matching(&report.lines, "could be listed to clean").is_empty(),
            "they were listed; it is the cleaning that did not happen: {:#?}",
            report.lines
        );
    }

    /// A second enumeration that would not list leaves `remaining` empty and
    /// says so in `trouble`. "Clean" is an empty `trouble`, never an empty
    /// `remaining`: the footer must not read a list we could not fetch as a
    /// machine with nothing left on it.
    #[test]
    fn a_package_pass_that_would_not_list_does_not_read_as_nothing_left() {
        let mut run = ordinary_run();
        let mut sweep = swept(1, 0);
        sweep.trouble = vec![
            "listing the packages after the removals would not start (0x80070422); what is left \
             is unknown"
                .to_string(),
        ];
        run.package = Some(sweep);
        let report = full(&run);
        let footer = report.lines[at(&report.lines, "done:")].to_string();

        assert_eq!(report.code, 0, "{:#?}", report.lines);
        assert!(!footer.contains("nothing left registered"), "{footer}");
        assert!(footer.contains("1 line(s) above"), "{footer}");
        // And the run does not add up to a clean machine, though no account had
        // anything to say.
        assert!(
            footer.contains("the package had something to report"),
            "{footer}"
        );
    }

    /// A regression guard, not a behavior test: a non-ASCII character written
    /// as UTF-8 (an em dash, say) does not survive the path from this
    /// process's stdout through the MSI log unchanged — CI proved as much,
    /// though exactly which link decodes it wrong is not pinned down. Every
    /// line this prints must stick to ASCII, so nobody has to find that link
    /// before the log reads right again.
    #[test]
    fn every_line_of_a_full_report_is_ascii() {
        let mut run = ordinary_run();
        run.privileges = Some("SeBackupPrivilege would not enable (error 1300)".to_string());
        run.trouble = vec![format!(
            "{BOB}: is not a SID this machine's ProfileList knows"
        )];
        run.skipped = 1;
        run.package = Some(swept(1, 1));

        run.accounts[0].hive.sweep = VerbSweep {
            removed: 16,
            absent: 0,
            refused: 2,
            lines: vec![
                SweepLine::Trouble(
                    r"SystemFileAssociations\.qoi\shell\Kuvatin: we are not allowed to open it (error 5)"
                        .to_string(),
                ),
                SweepLine::Note(
                    r"Directory\shell\Kuvatin: took a REG_LINK's entry without following it"
                        .to_string(),
                ),
                SweepLine::Refused(
                    r"image\shell\Kuvatin: we are not allowed to delete it (status 0xc0000022)"
                        .to_string(),
                ),
            ],
        };
        run.accounts[0].hive.trouble = vec![format!(
            r"HKEY_USERS\{ALICE}_Classes: we are not allowed to open it (error 5)"
        )];
        let (dirs, file_sweep) = done_of(&mut run.accounts[0]);
        *dirs = vec!["zzz-broken: we are not allowed to read it (error 5)".to_string()];
        file_sweep.trouble = vec![
            r"C:\Users\alice\AppData\Local\Kuvatin: it is a reparse point, so C:\Users\alice\AppData\Local\Kuvatin is not being deleted through it. A junction there needs no privilege to make, and this runs as SYSTEM"
                .to_string(),
        ];

        run.accounts[1].hive.access = HiveAccess::None;
        run.accounts[1].hive.trouble = vec![format!(
            r"HKEY_USERS\{BOB}_Classes: is not mounted and UsrClass.dat is not there"
        )];
        run.accounts[1].files = FileWork::OutOfTime;

        let lines = full(&run).lines;
        let not_ascii: Vec<&String> = lines.iter().filter(|line| !line.is_ascii()).collect();
        assert!(
            not_ascii.is_empty(),
            "non-ASCII report line(s): {not_ascii:#?}"
        );
    }
}
