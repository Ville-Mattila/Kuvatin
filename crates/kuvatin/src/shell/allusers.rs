//! The `--unregister-all-users` entry point: one pass that takes Kuvatin's
//! Explorer menu, its Windows 11 menu package and its leftover files off
//! **every** account on the machine.
//!
//! The installer runs this as SYSTEM, from a deferred custom action, while the
//! exe it is running still exists. Two rules follow from that, and they are the
//! shape of this module:
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
//!
//! The work is gathered first and printed afterwards, and that is what makes it
//! testable: [`gather`] is all the machine-touching and [`report`] is all the
//! wording — a pure function over what the other modules handed back. The tests
//! build those structs by hand and read the lines, so every rule about what
//! gets printed (the order, the counts, the cap on a hostile hive's hundred
//! refusals, which cases are failures and which are not) is pinned here rather
//! than left to be proven by running a real uninstall. Running a real one is
//! the release workflow's job.

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

/// Nothing was attempted: no account could be listed to clean.
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

/// What every line of the report opens with, so these lines can be picked out
/// of an MSI log carrying everything else the uninstall did.
const PREFIX: &str = "Kuvatin: ";

/// The first line: what this mode is, and what it is about to do.
const HEADLINE: &str = "--unregister-all-users: taking the Explorer menu, the Windows 11 menu \
                        package and the leftover files off every account on this machine.";

/// Clean every account, print what happened, and hand back the exit code.
///
/// Thin on purpose: it gathers, it words, it prints. Everything worth testing
/// is in the two halves it calls.
#[allow(dead_code)] // Called by main.rs's `--unregister-all-users` arm, a later task in this plan.
pub fn unregister_all_users() -> i32 {
    let report = report(&gather());
    for line in &report.lines {
        println!("{line}");
    }
    report.code
}

/// What one run gathered, before a word of it is printed.
///
/// Everything the report says comes from here, which is what lets the wording
/// be tested without a machine to clean.
#[derive(Debug, Default)]
pub(super) struct Run {
    /// Whether this process holds a full token. Without one nothing below was
    /// attempted, and every field after this one is empty.
    pub elevated: bool,
    /// What removing the package for every account came to. `None` when the run
    /// stopped before it.
    pub package: Option<PackageSweep>,
    /// Why `SeBackupPrivilege` and `SeRestorePrivilege` could not be enabled,
    /// when they could not. The run carries on regardless: a signed-in
    /// account's hive is mounted already and needs neither privilege.
    pub privileges: Option<String>,
    /// What `profiles::all()` could not read, exactly as it came — three kinds
    /// of trouble joined with `"; "`.
    pub listing: Option<String>,
    /// One entry per account the run visited.
    pub accounts: Vec<Account>,
}

/// What visiting one account came to: its classes hive, then its files.
#[derive(Debug)]
pub(super) struct Account {
    /// Its classes hive, and how that hive was reached.
    pub hive: HiveOutcome,
    /// What `paths::package_data_dirs` could not read under its `Packages`
    /// folder. Each line is already `<path>: <what happened>`.
    pub dirs: Vec<String>,
    /// Its files.
    pub files: FileSweep,
}

/// The report, ready to print, and what the process should exit with.
pub(super) struct Report {
    pub lines: Vec<String>,
    pub code: i32,
}

/// Do the work, touching the machine and printing nothing.
///
/// The order is the plan's, and it matters: the package goes first, while
/// `kuvatin.exe` and `kuvatin_shellext.dll` are still on disk, because removing
/// the registration is what lets go of the DLL before `RemoveFiles` comes to
/// delete it.
fn gather() -> Run {
    let elevated = hive::is_elevated();
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
    let (profiles, listing) = profiles::all();
    let accounts = profiles.iter().map(clean).collect();
    Run {
        elevated,
        package,
        privileges,
        listing,
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
    let (dirs, trouble) = paths::package_data_dirs(&profile.dir);
    let plan = paths::plan(&profile.dir, &dirs);
    // `profile.dir` itself and not a path built to look like it: the walk
    // strips this exact path off the front of every planned path, component by
    // component.
    let files = files::remove_plan(&profile.dir, &plan);
    Account {
        hive,
        dirs: trouble,
        files,
    }
}

/// Turn what a run gathered into the lines it prints and the code it exits
/// with. Pure: it reads the structs and touches nothing.
pub(super) fn report(run: &Run) -> Report {
    let mut out = Lines::default();
    out.say(HEADLINE);
    out.say(format!(
        "this process is elevated: {}.",
        if run.elevated { "yes" } else { "no" }
    ));
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
    out.say(format!(
        "a signed-out account's hive cannot be mounted: {why}"
    ));
    out.detail("a signed-in account's hive is mounted already, so those are still cleaned.");
}

/// What the account list came to: everything it could not read, a line each,
/// and then how many accounts there are to clean and how many were passed over.
///
/// The trouble is split back into lines exactly as `profiles::all()` joined it,
/// and printed with nothing in front of it — it mixes a ProfileList key path, a
/// sentence about a list with nothing in it, and one `<SID>: <reason>` per
/// account, so no one prefix would suit all three. (A reason that carries a
/// `"; "` of its own is split too, which reads as one line of reason and one of
/// consequence. That is the price of a joined string, and it costs nothing:
/// neither half starts with a SID, so neither is counted as an account.)
fn account_list_lines(run: &Run, out: &mut Lines) {
    let troubles = listing_lines(run);
    if !troubles.is_empty() {
        out.say("the account list did not read in full:");
        for line in &troubles {
            out.detail(line);
        }
    }
    // An account this run had to pass over is an account that keeps its menu,
    // so how many there were is part of the result rather than a detail. They
    // are the trouble lines that open with a SID: `profiles::cleanable_dir`
    // puts the account in front of its own reason, and the other two kinds of
    // trouble both open with the ProfileList key path.
    let skipped = troubles
        .iter()
        .filter(|line| line.starts_with("S-1-"))
        .count();
    match run.accounts.len() {
        // Every machine this could be uninstalled from has an account on it, so
        // none at all is the shape of an enumeration that went wrong rather
        // than of a machine with nobody on it.
        0 => out.say(format!(
            "no account to clean, which is not what a machine Kuvatin was installed on looks like; \
             {skipped} skipped."
        )),
        n => out.say(format!("{n} account(s) to clean; {skipped} skipped.")),
    }
}

/// The account list's trouble, back in the lines it was joined from.
fn listing_lines(run: &Run) -> Vec<&str> {
    run.listing
        .as_deref()
        .map(|trouble| trouble.split("; ").collect())
        .unwrap_or_default()
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

/// What came off one account's disk.
fn file_lines(account: &Account, out: &mut Lines) {
    let files = &account.files;
    out.detail(format!(
        "files: {} removed, {} already gone; {} folder tree(s) removed, {} already gone; \
         {} folder(s) pruned.",
        files.files_removed,
        files.files_absent,
        files.trees_removed,
        files.trees_absent,
        files.pruned
    ));
    // Both capped: a `Packages` folder holds names the account chose, and the
    // walk refuses each one it does not like by name.
    out.capped(&account.dirs);
    out.capped(&files.trouble);
}

/// The last two lines: what the run added up to, and what it exits with.
///
/// Exit 0 whenever the run happened, refusals and all — the uninstall does not
/// stop for them, and the log is where the detail belongs. A non-zero code says
/// one thing only: nothing was attempted. It is worth telling apart from a run
/// that attempted everything and was refused, because the answer to it is
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
        out.say(
            "nothing was attempted: no account to clean was listed, so no account's menu was \
             touched.",
        );
        out.say(format!(
            "exiting {EXIT_NO_ACCOUNTS}: nothing was attempted, because no account could be \
             listed to clean."
        ));
        return EXIT_NO_ACCOUNTS;
    }
    let told = run
        .accounts
        .iter()
        .filter(|account| {
            !account.hive.trouble.is_empty()
                || !account.dirs.is_empty()
                || !account.files.trouble.is_empty()
                || account.hive.sweep.refused > 0
                || !account.hive.sweep.troubles().is_empty()
        })
        .count();
    let aside = if told > 0 {
        format!("; {told} account(s) had something to report")
    } else {
        String::new()
    };
    out.say(format!(
        "done: {} account(s) visited; {} verb key(s) removed and {} refused; {} file(s), \
         {} folder tree(s) and {} folder(s) pruned; {}{aside}.",
        run.accounts.len(),
        total(run, |account| account.hive.sweep.removed),
        total(run, |account| account.hive.sweep.refused),
        total(run, |account| account.files.files_removed),
        total(run, |account| account.files.trees_removed),
        total(run, |account| account.files.pruned),
        package_total(run)
    ));
    out.say(format!(
        "exiting {EXIT_DONE}: the run finished. A refusal above does not stop the uninstall — the \
         detail is in this log."
    ));
    EXIT_DONE
}

fn total(run: &Run, of: impl Fn(&Account) -> usize) -> usize {
    run.accounts.iter().map(of).sum()
}

/// The package's share of the footer, in the same words the section above used
/// — an unsupported Windows is still not a failure down here.
fn package_total(run: &Run) -> String {
    match run.package.as_ref().map(|sweep| (sweep.reach, sweep)) {
        Some((PackageReach::Swept, sweep)) => format!(
            "the package removed for {} registration(s), {} still registered",
            sweep.removed,
            sweep.remaining.len()
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

    /// A run that went as well as a run can go: the package gone, two accounts
    /// cleaned, nothing refused.
    fn ordinary_run() -> Run {
        Run {
            elevated: true,
            package: Some(swept(1, 0)),
            privileges: None,
            listing: None,
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
            dirs: Vec::new(),
            files: FileSweep {
                files_removed: 3,
                files_absent: 0,
                trees_removed: 2,
                trees_absent: 0,
                pruned: 1,
                trouble: Vec::new(),
            },
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
        let report = report(&ordinary_run());
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
        let lines = report(&ordinary_run()).lines;
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
        let lines = report(&run).lines;

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
        run.listing = Some(format!(
            "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\ProfileList: we are not allowed to \
             open it (error 5); {BOB}: C:\\Users\\bob does not exist"
        ));
        let lines = report(&run).lines;

        assert_eq!(
            matching(&lines, "1 account(s) to clean").len(),
            1,
            "{lines:#?}"
        );
        assert_eq!(matching(&lines, "1 skipped").len(), 1, "{lines:#?}");
        // One line each, printed as they came, with no hive in front of them.
        assert_eq!(matching(&lines, "ProfileList: we are not allowed").len(), 1);
        assert_eq!(matching(&lines, "C:\\Users\\bob does not exist").len(), 1);
    }

    /// A machine Kuvatin was installed on has an account on it. None at all is
    /// the shape of a broken enumeration, and must not read as a clean sweep.
    #[test]
    fn no_account_at_all_is_suspicious_and_not_a_success() {
        let mut run = ordinary_run();
        run.accounts.clear();
        run.listing = Some(
            "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\ProfileList listed no accounts at \
             all, which no Windows machine does"
                .to_string(),
        );
        let report = report(&run);

        assert_eq!(report.code, 2, "{:#?}", report.lines);
        assert!(!matching(&report.lines, "no account to clean").is_empty());
        assert!(
            !matching(&report.lines, "nothing was attempted").is_empty(),
            "the footer must say why: {:#?}",
            report.lines
        );
    }

    /// A hostile hive can refuse hundreds of keys in a hundred different ways.
    /// The log says three and counts the rest.
    #[test]
    fn a_long_list_of_refusals_is_capped() {
        let mut run = ordinary_run();
        run.accounts.truncate(1);
        run.accounts[0].hive.sweep = VerbSweep {
            removed: 0,
            absent: 0,
            refused: 50,
            lines: (0..50)
                .map(|i| {
                    SweepLine::Refused(format!(
                        r"SystemFileAssociations\.x{i:03}\shell\Kuvatin: we are not allowed to delete it (status 0xc0000022)"
                    ))
                })
                .collect(),
        };
        let lines = report(&run).lines;

        assert_eq!(
            matching(&lines, "SystemFileAssociations").len(),
            3,
            "three refusals and no more: {lines:#?}"
        );
        assert_eq!(matching(&lines, "and 47 more").len(), 1, "{lines:#?}");
        // The count is still the whole truth, capped list or not.
        assert!(!matching(&lines, "50 refused").is_empty(), "{lines:#?}");
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
        let lines = report(&run).lines;

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
        let report = report(&run);

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
        let report = report(&run);

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
        run.accounts[0].files.trouble = vec![
            r"C:\Users\alice\AppData\Local\Kuvatin\kuvatin.log: we are not allowed to delete it (os error 5)"
                .to_string(),
        ];
        run.accounts[1].hive.trouble = vec![
            r"HKEY_USERS\S-1-5-21-x_Classes: we are not allowed to open it (error 5)".to_string(),
        ];
        let report = report(&run);

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
        let lines = report(&run).lines;

        assert!(!matching(&lines, "never reached").is_empty(), "{lines:#?}");
        assert!(matching(&lines, "around mounting").is_empty(), "{lines:#?}");
    }

    /// Without a full token there is no other account's hive to open and no
    /// package to remove for anybody, so nothing is attempted at all and the
    /// code says so.
    #[test]
    fn a_run_without_a_full_token_attempts_nothing() {
        let report = report(&Run::default());

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
        let report = report(&run);

        assert_eq!(report.code, 0);
        assert_eq!(matching(&report.lines, "SeBackupPrivilege").len(), 1);
        assert!(at(&report.lines, "SeBackupPrivilege") < at(&report.lines, "account(s) to clean"));
    }

    /// Package data folders live under a name the account chooses, so an
    /// unreadable `Packages` folder is reported per account, in its place.
    #[test]
    fn an_unreadable_packages_folder_is_reported_for_that_account() {
        let mut run = ordinary_run();
        run.accounts[0].dirs = vec![
            r"C:\Users\alice\AppData\Local\Packages would not be read (os error 5)".to_string(),
        ];
        let lines = report(&run).lines;

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
        let lines = report(&ordinary_run()).lines;
        let footer = lines[at(&lines, "done:")].to_string();

        assert!(footer.contains("2 account(s)"), "{footer}");
        assert!(footer.contains("34 verb key(s)"), "{footer}");
        assert!(footer.contains("6 file(s)"), "{footer}");
        assert!(footer.contains("4 folder tree(s)"), "{footer}");
        assert!(footer.contains("2 folder(s) pruned"), "{footer}");
        assert!(footer.contains("1 registration(s)"), "{footer}");
    }
}
