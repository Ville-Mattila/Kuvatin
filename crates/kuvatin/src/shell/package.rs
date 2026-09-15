//! The sparse package ("package with external location") that carries the
//! Windows 11 context-menu handler. Registering `Kuvatin.msix` with the install
//! directory as its external location gives that directory package identity,
//! which is the only way onto Windows 11's top-level context menu; the handler
//! itself is `kuvatin_shellext.dll` next to the exe (see `crates/kuvatin-shellext`).
//!
//! Registration is per user. The package must be signed with a certificate the
//! machine trusts: it declares a COM server ("executable activations"), which
//! Windows refuses in an unsigned package (0x80073D2B), and a signature whose
//! certificate is in neither the machine's Trusted People nor Trusted Root
//! store fails with 0x800B0109. Windows 10 cannot show the menu or register the
//! package, so everything here is skipped below build 22000, and every failure
//! is non-fatal: the classic registry menu keeps working regardless.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::time::{Duration, Instant};
use windows::core::{HRESULT, HSTRING};
use windows::Foundation::{AsyncStatus, IAsyncOperationWithProgress, Uri};
use windows::Management::Deployment::{
    AddPackageOptions, DeploymentProgress, DeploymentResult, PackageInstallState, PackageManager,
    RemovalOptions,
};
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// `Identity Name` in `crates/kuvatin/msix/AppxManifest.xml`.
pub const PACKAGE_NAME: &str = "VilleMattila.Kuvatin";
/// The package file, installed next to the exe.
pub const MSIX_FILE: &str = "Kuvatin.msix";
/// Windows 11: the first build with the new context menu.
const MIN_BUILD: u32 = 22000;

/// What one registration attempt concluded, for the log and the console.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Registered (or re-registered) now.
    Registered,
    /// This version at this location was already registered.
    AlreadyRegistered,
    /// Windows 10 (or older): no top-level menu to register into.
    Unsupported,
}

/// Windows build number from the registry (`GetVersionEx` lies to unmanifested
/// callers); 0 when unreadable.
pub fn windows_build() -> u32 {
    super::windows::read_hklm_string(
        r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
        "CurrentBuildNumber",
    )
    .and_then(|s| s.trim().parse().ok())
    .unwrap_or(0)
}

pub fn os_supports_package() -> bool {
    windows_build() >= MIN_BUILD
}

/// Join an apartment, and hand back what COM made of it — `S_OK`, the `S_FALSE`
/// of a thread already in one, or the `RPC_E_CHANGED_MODE` of a thread already
/// in an STA, all of which are fine here. Only the all-users sweep looks at the
/// answer, and only to say so when nothing else worked either.
fn init_com() -> HRESULT {
    // WinRT activation needs an apartment; MTA suits the blocking `.get()`
    // waits below. RPC_E_CHANGED_MODE (already STA) is fine too.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
}

/// `file:///C:/Program%20Files/...`: forward slashes, everything outside the
/// URI-unreserved set percent-encoded (UTF-8 bytes for non-ASCII).
fn file_uri_string(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut encoded = String::from("file:///");
    for b in text.trim_start_matches('/').bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                encoded.push(b as char)
            }
            _ => encoded.push_str(&format!("%{b:02X}")),
        }
    }
    encoded
}

fn file_uri(path: &Path) -> Result<Uri> {
    Uri::CreateUri(&HSTRING::from(file_uri_string(path))).context("file URI")
}

/// The registered package for the current user, as `(full name, version,
/// external location)`, if any.
pub fn registered() -> Result<Option<(String, String, String)>> {
    let _ = init_com();
    let pm = PackageManager::new()?;
    for p in pm.FindPackagesByUserSecurityId(&HSTRING::new())? {
        let id = p.Id()?;
        if id.Name()? != PACKAGE_NAME {
            continue;
        }
        let v = id.Version()?;
        let version = format!("{}.{}.{}", v.Major, v.Minor, v.Build);
        // The external location (the install dir), not the WindowsApps folder
        // that holds the package's own manifest.
        let location = p
            .EffectiveExternalPath()
            .map(|h| h.to_string())
            .unwrap_or_default();
        return Ok(Some((id.FullName()?.to_string(), version, location)));
    }
    Ok(None)
}

/// Register `install_dir\Kuvatin.msix` with `install_dir` as the external
/// location, replacing a registration of another version or location.
pub fn register(install_dir: &Path) -> Result<Outcome> {
    if !os_supports_package() {
        return Ok(Outcome::Unsupported);
    }
    let msix = install_dir.join(MSIX_FILE);
    if !msix.is_file() {
        bail!("{} is missing", msix.display());
    }
    let wanted = env!("CARGO_PKG_VERSION");
    if let Some((full, version, location)) = registered()? {
        if version == wanted && same_dir(&location, install_dir) {
            return Ok(Outcome::AlreadyRegistered);
        }
        // Same version elsewhere, or another version: Windows refuses to
        // re-register an identical version in place, so start clean.
        remove(&full)?;
    }
    let pm = PackageManager::new()?;
    let opts = AddPackageOptions::new()?;
    opts.SetExternalLocationUri(&file_uri(install_dir)?)?;
    let result = pm.AddPackageByUriAsync(&file_uri(&msix)?, &opts)?.get()?;
    let code = result.ExtendedErrorCode()?;
    if code.is_err() {
        bail!(
            "AddPackageByUriAsync: {} (0x{:08X})",
            result.ErrorText()?.to_string().trim(),
            code.0 as u32
        );
    }
    Ok(Outcome::Registered)
}

/// Remove the current user's registration, if any.
pub fn unregister() -> Result<bool> {
    if !os_supports_package() {
        return Ok(false);
    }
    match registered()? {
        Some((full, _, _)) => {
            remove(&full)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

fn remove(full_name: &str) -> Result<()> {
    let pm = PackageManager::new()?;
    let result = pm.RemovePackageAsync(&HSTRING::from(full_name))?.get()?;
    let code = result.ExtendedErrorCode()?;
    if code.is_err() {
        bail!(
            "RemovePackageAsync({full_name}): {} (0x{:08X})",
            result.ErrorText()?.to_string().trim(),
            code.0 as u32
        );
    }
    Ok(())
}

fn same_dir(a: &str, b: &Path) -> bool {
    let norm = |s: &str| s.trim_end_matches(['\\', '/']).to_lowercase();
    norm(a) == norm(&b.to_string_lossy())
}

// ---------------------------------------------------------------------------
// The package, for every account on the machine.
//
// Everything below runs as SYSTEM from the uninstaller's `--unregister-all-users`
// mode. It **reports**; it never prints and never logs. `crate::applog` as
// SYSTEM would resolve `%LOCALAPPDATA%` to the system profile and leave a
// brand-new file behind — exactly the sort of leftover this mode exists to
// remove — so what comes back here is the orchestrator's to print.
//
// Nothing outside `#[cfg(test)]` calls it yet: the caller is the
// `--unregister-all-users` entry point, a later task in that plan.
// ---------------------------------------------------------------------------

/// How long one deployment operation may take before the sweep stops waiting on
/// it, says so, and moves to the next.
///
/// There is no waiting without a deadline anywhere in here: this runs inside
/// `msiexec`, where a wait that never ends is an uninstall that never ends. A
/// removal normally takes seconds; three minutes is the generous end of "the
/// deployment service is having a hard time", not a number to be reached.
const DEPLOYMENT_TIMEOUT: Duration = Duration::from_secs(180);

/// How often the wait looks again. Small enough that a removal that finishes
/// quickly is noticed quickly, large enough that three minutes of looking costs
/// nothing.
const DEPLOYMENT_POLL: Duration = Duration::from_millis(250);

/// One account a registration belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Read by `unregister_all_users`'s caller: `--unregister-all-users`.
pub(super) struct PackageUser {
    /// The account's SID, as the deployment service spells it.
    pub sid: String,
    /// What it made of the package, in words: `installed`, `staged`, `paused`.
    pub state: String,
}

/// One registration of our package, and who it is registered for.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Read by `unregister_all_users`'s caller: `--unregister-all-users`.
pub(super) struct Registration {
    /// The package full name, which is what a removal is asked for by.
    pub full_name: String,
    /// The accounts it is registered for. Empty when the deployment service
    /// would not say — the reason is then in the sweep's `trouble`.
    pub users: Vec<PackageUser>,
}

impl Registration {
    /// How a registration that outlived the removals reads: one line per
    /// account, each naming the package and the account, and one line anyway
    /// when no account could be named — a registration belonging to nobody we
    /// can name is still a leftover, and must not read as a clean machine.
    fn still_registered_lines(&self) -> Vec<String> {
        if self.users.is_empty() {
            return vec![format!(
                "{}: still registered, for no account the deployment service would name",
                self.full_name
            )];
        }
        self.users
            .iter()
            .map(|user| {
                format!(
                    "{}: still registered for {} ({})",
                    self.full_name, user.sid, user.state
                )
            })
            .collect()
    }
}

/// What one removal came to.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Built by `unregister_all_users`, for `--unregister-all-users`.
pub(super) enum Removal {
    /// The deployment service reported no error.
    Gone,
    /// It did, or it never answered. The sentence is fit to print after the
    /// package's own name.
    Refused(String),
}

/// How far the sweep got before it had anything to sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Read by `unregister_all_users`'s caller: `--unregister-all-users`.
pub(super) enum PackageReach {
    /// The deployment service answered and the sweep ran. Whether it found
    /// anything is `found`'s business.
    Swept,
    /// This Windows predates the package. Nothing of the sort can be registered
    /// and nothing went wrong.
    Unsupported,
    /// The deployment service would not answer, so nothing was attempted and
    /// whatever is registered still is. `trouble` says why.
    Unreachable,
}

impl PackageReach {
    /// How the uninstall should say this, so whoever prints does not have to
    /// work it out again.
    #[allow(dead_code)] // Printed by `--unregister-all-users`.
    pub(super) fn wording(&self) -> &'static str {
        match self {
            PackageReach::Swept => "the deployment service answered",
            PackageReach::Unsupported => {
                "this Windows predates the package; there is nothing of the sort to remove"
            }
            PackageReach::Unreachable => {
                "the deployment service would not answer; nothing was attempted"
            }
        }
    }
}

/// What removing the package for every account came to, ready for the uninstall
/// to print. Nothing here has been printed or logged.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Read by `unregister_all_users`'s caller: `--unregister-all-users`.
pub(super) struct PackageSweep {
    /// How far the sweep got.
    pub reach: PackageReach,
    /// Every registration there was before the removals, with the accounts each
    /// belonged to.
    pub found: Vec<Registration>,
    /// How many of those the deployment service said it removed.
    pub removed: usize,
    /// What was still registered afterwards — asked of the deployment service
    /// again, never inferred from the removals. Empty is the whole point.
    pub remaining: Vec<Registration>,
    /// Every refusal, every error and every leftover, each a sentence meant to
    /// be printed as it stands.
    pub trouble: Vec<String>,
}

impl PackageSweep {
    /// The report of a sweep that never ran: `reach` says why, and there is
    /// nothing else to say.
    fn nothing_attempted(reach: PackageReach, trouble: Vec<String>) -> Self {
        PackageSweep {
            reach,
            found: Vec::new(),
            removed: 0,
            remaining: Vec::new(),
            trouble,
        }
    }
}

/// Every registration found in one pass, and the package family they share.
/// The family name is what deprovisioning asks for, and every registration of
/// one package has the same one, so the first that reads is as good as any.
type Found = (Vec<Registration>, Option<String>);

/// Remove the sparse package for **every** account on the machine.
///
/// Runs as SYSTEM, where `PackageManager::FindPackages` may enumerate other
/// accounts' packages and `RemovalOptions::RemoveForAllUsers` may remove them.
/// Best effort throughout: every failure is recorded and the rest carries on,
/// because a second account's registration is no less worth removing for the
/// first one having gone wrong.
#[allow(dead_code)] // The caller is the `--unregister-all-users` entry point, a later task.
pub(super) fn unregister_all_users() -> PackageSweep {
    if !os_supports_package() {
        return PackageSweep::nothing_attempted(PackageReach::Unsupported, Vec::new());
    }
    let apartment = init_com();
    let pm = match PackageManager::new() {
        Ok(pm) => pm,
        Err(e) => {
            return PackageSweep::nothing_attempted(
                PackageReach::Unreachable,
                vec![format!(
                    "the deployment service would not start ({e}){}",
                    apartment_aside(apartment)
                )],
            )
        }
    };

    let mut trouble = Vec::new();
    let (found, family) = find_registrations(&pm, "before the removals", &mut trouble);
    // Only if it really is provisioned machine-wide. Kuvatin's installer never
    // provisions it, so this is a question that costs one call and answers no.
    if let Some(family) = &family {
        deprovision_if_provisioned(&pm, family, &mut trouble);
    }
    let removals = found
        .iter()
        .map(|registration| {
            (
                registration.full_name.clone(),
                remove_for_all_users(&pm, &registration.full_name),
            )
        })
        .collect();
    // Asked again rather than assumed: a removal the service called a success
    // can still leave an account in a staged state, and that is what the
    // uninstall needs to hear about.
    let (remaining, _) = find_registrations(&pm, "after the removals", &mut trouble);
    gather(PackageReach::Swept, found, removals, remaining, trouble)
}

/// Fold what the deployment service said into the report: count what went, turn
/// every refusal into a line naming the package, and turn everything still
/// registered into a line naming the package and the account.
///
/// Pure, and the whole shape of the report is decided here — so the shape can
/// be tested without a package to remove, which on a developer's machine (or a
/// machine running Kuvatin) is the only honest way to test it at all.
fn gather(
    reach: PackageReach,
    found: Vec<Registration>,
    removals: Vec<(String, Removal)>,
    remaining: Vec<Registration>,
    mut trouble: Vec<String>,
) -> PackageSweep {
    let mut removed = 0;
    for (full_name, outcome) in removals {
        match outcome {
            Removal::Gone => removed += 1,
            Removal::Refused(why) => trouble.push(format!("{full_name}: {why}")),
        }
    }
    for still in &remaining {
        trouble.extend(still.still_registered_lines());
    }
    PackageSweep {
        reach,
        found,
        removed,
        remaining,
        trouble,
    }
}

/// Every registration of our package, for every account. `when` says which pass
/// this is, so two failures do not read as one repeated.
///
/// `FindPackages` with no SID is every account's packages, which Windows allows
/// only to an administrator — SYSTEM is one. An ordinary token is refused, and
/// the refusal goes into `trouble` rather than passing for a machine with
/// nothing registered.
fn find_registrations(pm: &PackageManager, when: &str, trouble: &mut Vec<String>) -> Found {
    let mut registrations: Vec<Registration> = Vec::new();
    let mut family: Option<String> = None;
    let packages = match pm.FindPackages() {
        Ok(packages) => packages,
        Err(e) => {
            trouble.push(format!(
                "the machine's packages could not be listed {when} ({e})"
            ));
            return (registrations, family);
        }
    };
    for package in packages {
        // A package whose id will not read cannot be shown to be ours, and
        // saying so for every other vendor's package on the machine would bury
        // what this report is for.
        let Ok(id) = package.Id() else { continue };
        if id.Name().map(|name| name != PACKAGE_NAME).unwrap_or(true) {
            continue;
        }
        let full_name = match id.FullName() {
            Ok(full_name) => full_name.to_string(),
            Err(e) => {
                trouble.push(format!(
                    "a registration of {PACKAGE_NAME} would not give its full name {when} ({e})"
                ));
                continue;
            }
        };
        if family.is_none() {
            family = id.FamilyName().ok().map(|name| name.to_string());
        }
        // One package registered for several accounts comes back once per
        // account; the accounts are `FindUsers`'s to list, not this loop's.
        if registrations.iter().any(|seen| seen.full_name == full_name) {
            continue;
        }
        let users = users_of(pm, &full_name, when, trouble);
        registrations.push(Registration { full_name, users });
    }
    (registrations, family)
}

/// The accounts one registration belongs to, and what state each made of it.
fn users_of(
    pm: &PackageManager,
    full_name: &str,
    when: &str,
    trouble: &mut Vec<String>,
) -> Vec<PackageUser> {
    let found = match pm.FindUsers(&HSTRING::from(full_name)) {
        Ok(found) => found,
        Err(e) => {
            trouble.push(format!(
                "{full_name}: the accounts it is registered for could not be listed {when} ({e})"
            ));
            return Vec::new();
        }
    };
    found
        .into_iter()
        .map(|user| PackageUser {
            sid: match user.UserSecurityId() {
                Ok(sid) if !sid.is_empty() => sid.to_string(),
                _ => "an account the deployment service would not name".to_string(),
            },
            state: user
                .InstallState()
                .map(state_wording)
                .unwrap_or_else(|e| format!("its state would not read ({e})")),
        })
        .collect()
}

/// How an install state reads in a report.
fn state_wording(state: PackageInstallState) -> String {
    match state {
        PackageInstallState::NotInstalled => "not installed".to_string(),
        PackageInstallState::Staged => "staged".to_string(),
        PackageInstallState::Installed => "installed".to_string(),
        PackageInstallState::Paused => "paused".to_string(),
        // A state this Windows knows and this build does not: say the number
        // rather than pretend to recognise it.
        other => format!("state {}", other.0),
    }
}

/// Deprovision the package family, but only when it really is provisioned for
/// all users.
///
/// Nothing in Kuvatin's install provisions it — registration is per user, from
/// the app itself — so this finds nothing today and costs one call for the day
/// something changes. Microsoft's per-machine uninstall order is deprovision
/// first, remove second, and that is the order here.
fn deprovision_if_provisioned(pm: &PackageManager, family: &str, trouble: &mut Vec<String>) {
    let provisioned = match pm.FindProvisionedPackages() {
        Ok(provisioned) => provisioned.into_iter().any(|package| {
            package
                .Id()
                .and_then(|id| id.FamilyName())
                .map(|name| name == family)
                .unwrap_or(false)
        }),
        Err(e) => {
            trouble.push(format!(
                "the machine's provisioned packages could not be listed ({e})"
            ));
            return;
        }
    };
    if !provisioned {
        return;
    }
    let started = pm.DeprovisionPackageForAllUsersAsync(&HSTRING::from(family));
    let why = match started {
        Ok(op) => match await_deployment("deprovisioning", &op) {
            Ok(result) => verdict("deprovisioning", &result).err(),
            Err(why) => Some(why),
        },
        Err(e) => Some(format!("deprovisioning would not start ({e})")),
    };
    if let Some(why) = why {
        trouble.push(format!("{family}: {why}"));
    }
}

/// Remove one registration for every account that holds it.
fn remove_for_all_users(pm: &PackageManager, full_name: &str) -> Removal {
    let started = pm.RemovePackageWithOptionsAsync(
        &HSTRING::from(full_name),
        RemovalOptions::RemoveForAllUsers,
    );
    let op = match started {
        Ok(op) => op,
        Err(e) => return Removal::Refused(format!("the removal would not start ({e})")),
    };
    match await_deployment("the removal", &op) {
        Ok(result) => match verdict("the removal", &result) {
            Ok(()) => Removal::Gone,
            Err(why) => Removal::Refused(why),
        },
        Err(why) => Removal::Refused(why),
    }
}

/// Wait for a deployment operation and hand back its result, for at most
/// [`DEPLOYMENT_TIMEOUT`].
///
/// The wait **polls `Status()`** rather than waiting on a completion handler,
/// and deliberately: a handler has to be marshalled back into the apartment
/// that set it, which in an STA means a message pump this process does not run
/// — so a handler-based wait could sit out the whole deadline for no better
/// reason than the apartment it was called from. `Status()` is a direct call on
/// the operation and answers in any apartment. The cost is a look every
/// [`DEPLOYMENT_POLL`], which is nothing next to a deployment.
fn await_deployment(
    what: &str,
    op: &IAsyncOperationWithProgress<DeploymentResult, DeploymentProgress>,
) -> Result<DeploymentResult, String> {
    wait_for(what, DEPLOYMENT_TIMEOUT, DEPLOYMENT_POLL, || {
        match op.Status() {
            Ok(AsyncStatus::Started) => Ok(false),
            // Completed, errored or cancelled: `GetResults` has the rest of it.
            Ok(_) => Ok(true),
            Err(e) => Err(format!("{what} could not be asked how it was going ({e})")),
        }
    })?;
    // An operation that ended in an error throws it from here rather than
    // handing back a result to read, so both ways of failing are covered.
    op.GetResults().map_err(|e| format!("{what} failed ({e})"))
}

/// Look until `finished` says yes, for at most `timeout`, looking again every
/// `tick`.
///
/// The look comes before the deadline check, so an operation that has already
/// finished is never reported as having run out of time. Split out from the
/// WinRT call it waits on so that the deadline arithmetic — the only part that
/// can be wrong without a package in hand — is testable.
fn wait_for(
    what: &str,
    timeout: Duration,
    tick: Duration,
    mut finished: impl FnMut() -> Result<bool, String>,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if finished()? {
            return Ok(());
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!(
                "{what} was still going after {timeout:?}; the uninstall waited no longer"
            ));
        }
        // Never past the deadline, so the last look happens on time rather than
        // a whole tick late.
        std::thread::sleep(tick.min(left));
    }
}

/// What the deployment service made of an operation: `Ok` when it reports no
/// error, and its own words and code when it does.
fn verdict(what: &str, result: &DeploymentResult) -> Result<(), String> {
    match result.ExtendedErrorCode() {
        Ok(code) if code.is_ok() => Ok(()),
        Ok(code) => {
            let text = result
                .ErrorText()
                .map(|text| text.to_string())
                .unwrap_or_default();
            Err(format!(
                "{what} reported {}",
                error_sentence(&text, code.0 as u32)
            ))
        }
        Err(e) => Err(format!("{what}'s result would not be read ({e})")),
    }
}

/// A deployment failure in one sentence: what it said, and which failure it
/// was. Deployment errors do not always carry words, and a bare code is still
/// worth printing — it is what a support mail can be searched for.
fn error_sentence(text: &str, code: u32) -> String {
    match text.trim() {
        "" => format!("0x{code:08X}"),
        said => format!("{said} (0x{code:08X})"),
    }
}

/// What to add to a failure sentence when COM itself would not start. `S_OK`,
/// the `S_FALSE` of a thread already in an apartment and the
/// `RPC_E_CHANGED_MODE` of a thread already in an STA are all ordinary here —
/// the wait above polls and so minds none of them — and say nothing.
fn apartment_aside(apartment: HRESULT) -> String {
    if apartment.is_ok() || apartment == RPC_E_CHANGED_MODE {
        String::new()
    } else {
        format!(
            "; COM would not start either (0x{:08X})",
            apartment.0 as u32
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::hive::is_elevated;
    use super::super::test_support::skip_or_fail_on_ci;
    use super::*;
    use std::cell::Cell;

    fn user(sid: &str, state: &str) -> PackageUser {
        PackageUser {
            sid: sid.to_string(),
            state: state.to_string(),
        }
    }

    fn registration(full_name: &str, users: &[PackageUser]) -> Registration {
        Registration {
            full_name: full_name.to_string(),
            users: users.to_vec(),
        }
    }

    /// The whole report, over data that no package had to be removed to
    /// produce: what went is counted, what would not go is named, and what
    /// outlived the removals is named once per account holding it.
    #[test]
    fn a_sweep_counts_what_went_and_names_what_did_not() {
        let gone = registration(
            "VilleMattila.Kuvatin_2.9.1.0_x64__8wekyb3d8bbwe",
            &[user("S-1-5-21-1", "installed")],
        );
        let stuck = registration(
            "VilleMattila.Kuvatin_2.8.0.0_x64__8wekyb3d8bbwe",
            &[
                user("S-1-5-21-2", "staged"),
                user("S-1-5-21-3", "installed"),
            ],
        );
        let sweep = gather(
            PackageReach::Swept,
            vec![gone.clone(), stuck.clone()],
            vec![
                (gone.full_name.clone(), Removal::Gone),
                (
                    stuck.full_name.clone(),
                    Removal::Refused("the removal was still going after 180s".to_string()),
                ),
            ],
            vec![stuck.clone()],
            vec!["something earlier went wrong".to_string()],
        );

        assert_eq!(sweep.removed, 1, "one of the two went");
        assert_eq!(sweep.found, vec![gone.clone(), stuck.clone()]);
        assert_eq!(sweep.remaining, vec![stuck.clone()]);
        // What came in stays, and stays first.
        assert_eq!(sweep.trouble[0], "something earlier went wrong");
        // The refusal names the package that refused.
        assert!(
            sweep
                .trouble
                .iter()
                .any(|line| line.starts_with(&stuck.full_name)
                    && line.contains("still going after 180s")),
            "no refusal line naming the package: {:?}",
            sweep.trouble
        );
        // And a line per account it is still registered for, naming both.
        for sid in ["S-1-5-21-2", "S-1-5-21-3"] {
            assert!(
                sweep
                    .trouble
                    .iter()
                    .any(|line| line.contains(&stuck.full_name)
                        && line.contains(sid)
                        && line.contains("still registered")),
                "nothing said about {sid}: {:?}",
                sweep.trouble
            );
        }
        // Nothing at all about the one that went.
        assert!(
            !sweep
                .trouble
                .iter()
                .any(|line| line.contains(&gone.full_name)),
            "the removed package should not be mentioned: {:?}",
            sweep.trouble
        );
    }

    #[test]
    fn a_sweep_that_removed_everything_has_nothing_to_report() {
        let one = registration(
            "VilleMattila.Kuvatin_2.9.1.0_x64__8wekyb3d8bbwe",
            &[user("S-1-5-21-1", "installed")],
        );
        let sweep = gather(
            PackageReach::Swept,
            vec![one.clone()],
            vec![(one.full_name.clone(), Removal::Gone)],
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(sweep.removed, 1);
        assert!(sweep.remaining.is_empty());
        assert!(sweep.trouble.is_empty(), "{:?}", sweep.trouble);
    }

    /// A registration the deployment service will not name an owner for is
    /// still a leftover, and must not read as a clean machine.
    #[test]
    fn a_registration_left_behind_for_nobody_is_still_said_out_loud() {
        let orphan = registration("VilleMattila.Kuvatin_2.9.1.0_x64__8wekyb3d8bbwe", &[]);
        let sweep = gather(
            PackageReach::Swept,
            vec![orphan.clone()],
            vec![(orphan.full_name.clone(), Removal::Gone)],
            vec![orphan.clone()],
            Vec::new(),
        );
        assert_eq!(sweep.trouble.len(), 1, "{:?}", sweep.trouble);
        assert!(
            sweep.trouble[0].contains(&orphan.full_name)
                && sweep.trouble[0].contains("still registered"),
            "{}",
            sweep.trouble[0]
        );
    }

    #[test]
    fn a_sweep_that_never_ran_says_why_and_counts_nothing() {
        let old = PackageSweep::nothing_attempted(PackageReach::Unsupported, Vec::new());
        assert_eq!(old.removed, 0);
        assert!(old.found.is_empty() && old.remaining.is_empty());
        // Windows 10 is not a fault: there is nothing of the sort to remove.
        assert!(old.trouble.is_empty());
        assert!(
            old.reach.wording().contains("predates"),
            "{}",
            old.reach.wording()
        );

        let dead = PackageSweep::nothing_attempted(
            PackageReach::Unreachable,
            vec!["the deployment service would not start".to_string()],
        );
        assert_eq!(dead.trouble.len(), 1);
        assert!(
            dead.reach.wording().contains("nothing was attempted"),
            "{}",
            dead.reach.wording()
        );
    }

    #[test]
    fn a_deployment_error_reads_with_its_own_words_and_its_code() {
        assert_eq!(
            error_sentence("  Access is denied.  ", 0x8007_0005),
            "Access is denied. (0x80070005)"
        );
        // Deployment failures do not always carry words; the code still says
        // which failure it was.
        assert_eq!(error_sentence("   ", 0x8007_0005), "0x80070005");
    }

    #[test]
    fn a_bounded_wait_stops_as_soon_as_the_work_is_done() {
        let looks = Cell::new(0);
        wait_for(
            "the removal",
            Duration::from_secs(60),
            Duration::from_millis(1),
            || {
                looks.set(looks.get() + 1);
                Ok(looks.get() == 3)
            },
        )
        .expect("it finished well inside the deadline");
        assert_eq!(looks.get(), 3, "it must stop looking the moment it is done");
    }

    /// An operation that has already finished needs no time at all, so the
    /// deadline is checked after the look, never before it.
    #[test]
    fn a_bounded_wait_looks_once_even_with_no_time_left() {
        let looks = Cell::new(0);
        wait_for(
            "the removal",
            Duration::ZERO,
            Duration::from_millis(1),
            || {
                looks.set(looks.get() + 1);
                Ok(true)
            },
        )
        .expect("an operation already finished needs no waiting");
        assert_eq!(looks.get(), 1);
    }

    #[test]
    fn a_bounded_wait_gives_up_and_says_what_was_going_and_for_how_long() {
        let looks = Cell::new(0);
        let why = wait_for(
            "the removal",
            Duration::from_millis(40),
            Duration::from_millis(5),
            || {
                looks.set(looks.get() + 1);
                Ok(false)
            },
        )
        .expect_err("it must give up rather than wait inside msiexec forever");
        assert!(
            why.starts_with("the removal was still going after 40ms"),
            "{why}"
        );
        assert!(
            looks.get() > 1,
            "40ms of 5ms ticks should be more than one look: {}",
            looks.get()
        );
    }

    #[test]
    fn a_bounded_wait_hands_back_a_status_it_could_not_read() {
        let why = wait_for(
            "the removal",
            Duration::from_secs(60),
            Duration::from_millis(1),
            || Err("its status would not read".to_string()),
        )
        .expect_err("an unreadable status ends the wait");
        assert_eq!(why, "its status would not read");
    }

    #[test]
    fn install_states_read_as_words() {
        assert_eq!(state_wording(PackageInstallState::Installed), "installed");
        assert_eq!(state_wording(PackageInstallState::Staged), "staged");
        assert_eq!(
            state_wording(PackageInstallState::NotInstalled),
            "not installed"
        );
        assert_eq!(state_wording(PackageInstallState::Paused), "paused");
        // A state this Windows knows and this build does not still prints.
        assert_eq!(state_wording(PackageInstallState(7)), "state 7");
    }

    /// The real enumeration against the real machine, removing nothing: ask the
    /// deployment service for every account's packages and check that what the
    /// filter hands back is ours and nobody else's.
    ///
    /// Listing other accounts' packages is an administrator's call, so an
    /// unelevated process has nothing to measure and says it is skipping. CI's
    /// runner is elevated, and a skip there would mean this never ran at all.
    #[test]
    fn enumerating_every_account_hands_back_only_our_package() {
        if !is_elevated() {
            skip_or_fail_on_ci("not elevated");
            return;
        }
        let _ = init_com();
        let pm = PackageManager::new().expect("a PackageManager");
        let mut trouble = Vec::new();
        let (found, family) = find_registrations(&pm, "in a test", &mut trouble);
        assert!(
            trouble.is_empty(),
            "an elevated enumeration has nothing to complain about: {trouble:?}"
        );
        let ours = format!("{PACKAGE_NAME}_");
        for registration in &found {
            assert!(
                registration.full_name.starts_with(&ours),
                "{} is not one of ours",
                registration.full_name
            );
            for user in &registration.users {
                assert!(user.sid.starts_with("S-1-"), "{} is not a SID", user.sid);
            }
        }
        if let Some(family) = &family {
            assert!(family.starts_with(&ours), "{family} is not our family");
        }
    }

    #[test]
    fn file_uris_escape_spaces_and_use_forward_slashes() {
        assert_eq!(
            file_uri_string(Path::new(r"C:\Program Files\kuvatin\bin")),
            "file:///C:/Program%20Files/kuvatin/bin"
        );
        assert_eq!(
            file_uri_string(Path::new(r"D:\Työt\Kuvatin.msix")),
            "file:///D:/Ty%C3%B6t/Kuvatin.msix"
        );
        // And WinRT accepts the result (it may re-render it differently).
        assert!(file_uri(Path::new(r"C:\Program Files\kuvatin\bin")).is_ok());
    }

    #[test]
    fn same_dir_ignores_case_and_trailing_separators() {
        assert!(same_dir(
            r"C:\Program Files\Kuvatin\bin\",
            Path::new(r"c:\program files\kuvatin\bin")
        ));
        assert!(!same_dir(
            r"C:\Other",
            Path::new(r"C:\Program Files\kuvatin\bin")
        ));
    }

    #[test]
    fn windows_build_is_a_real_number_here() {
        assert!(windows_build() >= 10240, "got {}", windows_build());
    }
}
