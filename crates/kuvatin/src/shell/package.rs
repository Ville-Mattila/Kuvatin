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
use windows::core::{RuntimeType, HRESULT, HSTRING};
use windows::ApplicationModel::Package;
use windows::Foundation::Collections::IIterable;
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
    // WinRT activation needs an apartment. The MTA is asked for because the
    // per-user path below blocks in `.get()`, which an STA would have to pump
    // for; the all-users sweep polls instead and minds no apartment at all. So
    // RPC_E_CHANGED_MODE — this thread is already in an STA — is fine either
    // way, and so is the S_FALSE of a thread already in an apartment.
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
    let packages = pm.FindPackagesByUserSecurityId(&HSTRING::new())?;
    // Walked by hand like every other enumeration here, rather than with the
    // crate's `IntoIterator` for `&IIterable`, which is `First().unwrap()`
    // (see [`walk`]). The walk runs to the end even once ours is in hand:
    // stopping early is not something `walk` offers, and one account's
    // packages are a short list.
    let mut ours: windows::core::Result<Option<(String, String, String)>> = Ok(None);
    let walked = walk(&packages, |package| {
        // The first answer stands, whether it is ours or a read that failed.
        if matches!(ours, Ok(None)) {
            ours = ours_registration(&package);
        }
    });
    if let Err(why) = walked {
        bail!("this account's packages could not be walked to the end: {why}");
    }
    Ok(ours?)
}

/// `package` as `(full name, version, external location)` when it is ours;
/// `None` when it belongs to somebody else.
fn ours_registration(package: &Package) -> windows::core::Result<Option<(String, String, String)>> {
    let id = package.Id()?;
    if id.Name()? != PACKAGE_NAME {
        return Ok(None);
    }
    let v = id.Version()?;
    let version = format!("{}.{}.{}", v.Major, v.Minor, v.Build);
    // The external location (the install dir), not the WindowsApps folder
    // that holds the package's own manifest.
    let location = package
        .EffectiveExternalPath()
        .map(|h| h.to_string())
        .unwrap_or_default();
    Ok(Some((id.FullName()?.to_string(), version, location)))
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
// ---------------------------------------------------------------------------

/// How long one deployment operation may take before the sweep stops waiting on
/// it, says so, and moves to the next.
///
/// There is no waiting without a deadline anywhere in here: this runs inside
/// `msiexec`, where a wait that never ends is an uninstall that never ends. A
/// removal normally takes seconds; three minutes is the generous end of "the
/// deployment service is having a hard time", not a number to be reached.
///
/// It is **per operation**: one deprovisioning, then one removal per distinct
/// package full name, so N full names is a worst case of (N + 1) × three
/// minutes. N is 1 in practice — a full name is one version of one package, and
/// only a machine that registered several versions and never removed one has
/// more.
///
/// A wait that runs out **abandons** the operation; it does not cancel it. The
/// removal is the deployment service's own work and carries on after we stop
/// watching, so a late one still removes the package. `Cancel()` would risk the
/// opposite — an account left `Staged`, half-removed, which is worse than late
/// — and `IAsyncInfo::Cancel` is itself a call that can block, which is the one
/// thing that must not happen here.
const DEPLOYMENT_TIMEOUT: Duration = Duration::from_secs(180);

/// How often the wait looks again. Small enough that a removal that finishes
/// quickly is noticed quickly, large enough that three minutes of looking costs
/// nothing.
const DEPLOYMENT_POLL: Duration = Duration::from_millis(250);

/// One account a registration belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PackageUser {
    /// The account's SID, as the deployment service spells it.
    pub sid: String,
    /// What it made of the package, in words: `installed`, `staged`, `paused`.
    pub state: String,
}

/// One registration of our package, and who it is registered for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Registration {
    /// The package full name, which is what a removal is asked for by.
    pub full_name: String,
    /// The accounts it is registered for. Empty when the deployment service
    /// would not say, and short of them all when the walk over them broke
    /// partway — either way the reason is in the sweep's `trouble`, and a list
    /// that is empty or short is never proof that no other account has it.
    pub users: Vec<PackageUser>,
}

impl Registration {
    /// How a registration that outlived the removals reads: one line per
    /// account, each naming the package and the account, and one line anyway
    /// when no account could be named — a registration belonging to nobody we
    /// can name is still a leftover, and must not read as a clean machine.
    ///
    /// `still_going` says its removal was still running when the uninstall
    /// stopped waiting. That is worth tying to the leftover: the two lines are
    /// otherwise a failure and an unexplained remnant, when in truth they are
    /// one thing that had not finished yet.
    fn still_registered_lines(&self, still_going: bool) -> Vec<String> {
        let aside = if still_going {
            "; its removal was still going when the uninstall stopped waiting, so this may yet clear"
        } else {
            ""
        };
        if self.users.is_empty() {
            return vec![format!(
                "{}: still registered, for no account the deployment service would name{aside}",
                self.full_name
            )];
        }
        self.users
            .iter()
            .map(|user| {
                format!(
                    "{}: still registered for {} ({}){aside}",
                    self.full_name, user.sid, user.state
                )
            })
            .collect()
    }
}

/// What one removal came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Removal {
    /// The deployment service reported no error.
    Gone,
    /// It reported one, or it could not be asked. The sentence is fit to print
    /// after the package's own name.
    Refused(String),
    /// The deadline passed with the removal still running. Not a refusal: the
    /// deployment service is still at it, and the package may well go a moment
    /// after the uninstall has stopped watching.
    StillGoing(String),
}

/// What the removals as a whole came to.
struct Removals {
    /// How many the deployment service said it removed.
    removed: usize,
    /// The full names whose removal was still running when the deadline passed,
    /// so that a leftover belonging to one of them can say as much.
    still_going: Vec<String>,
}

/// How far the sweep got before it had anything to sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
pub(super) struct PackageSweep {
    /// How far the sweep got.
    pub reach: PackageReach,
    /// Every registration there was before the removals, with the accounts each
    /// belonged to.
    pub found: Vec<Registration>,
    /// How many of those the deployment service said it removed.
    pub removed: usize,
    /// What was still registered afterwards — asked of the deployment service
    /// again, never inferred from the removals. Empty is the whole point, but
    /// it is only proof of a clean machine when `trouble` carries no line about
    /// the second pass: a pass that would not list leaves this empty too, and
    /// says so there.
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

/// Every registration one pass listed, and the package family they share. The
/// family name is what deprovisioning asks for, and every registration of one
/// package has the same one, so the first that reads is as good as any.
type Found = (Vec<Registration>, Option<String>);

/// Remove the sparse package for **every** account on the machine.
///
/// Runs as SYSTEM, where `PackageManager::FindPackages` may enumerate other
/// accounts' packages and `RemovalOptions::RemoveForAllUsers` may remove them.
/// Best effort throughout: every failure is recorded and the rest carries on,
/// because a second account's registration is no less worth removing for the
/// first one having gone wrong.
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
    // A first pass that would not list is not a machine with nothing
    // registered: it is a machine we cannot speak for. Calling that a sweep
    // would have the orchestrator print that the deployment service answered,
    // when what it did was refuse.
    let (found, family) = match find_registrations(&pm, "before the removals", &mut trouble) {
        Ok(listed) => listed,
        Err(why) => {
            trouble.push(why);
            return PackageSweep::nothing_attempted(PackageReach::Unreachable, trouble);
        }
    };
    deprovision_provisioned(&pm, family.as_deref(), &mut trouble);
    let removals = note_removals(
        found
            .iter()
            .map(|registration| {
                (
                    registration.full_name.clone(),
                    remove_for_all_users(&pm, &registration.full_name),
                )
            })
            .collect(),
        &mut trouble,
    );
    // Asked again rather than assumed: a removal the service called a success
    // can still leave an account in a staged state, and that is what the
    // uninstall needs to hear about.
    let remaining = match find_registrations(&pm, "after the removals", &mut trouble) {
        Ok((remaining, _)) => remaining,
        Err(why) => {
            trouble.push(format!("{why}; what is left is unknown"));
            Vec::new()
        }
    };
    gather(found, removals, remaining, trouble)
}

/// Count the removals and say what went wrong with each, in the order they were
/// tried.
///
/// Called where the removals happen rather than at the end, so that `trouble`
/// reads in the order the uninstall lived it: what the first pass had to say,
/// then the removals, then the second pass.
fn note_removals(removals: Vec<(String, Removal)>, trouble: &mut Vec<String>) -> Removals {
    let mut noted = Removals {
        removed: 0,
        still_going: Vec::new(),
    };
    for (full_name, outcome) in removals {
        match outcome {
            Removal::Gone => noted.removed += 1,
            Removal::Refused(why) => trouble.push(format!("{full_name}: {why}")),
            Removal::StillGoing(why) => {
                trouble.push(format!("{full_name}: {why}"));
                noted.still_going.push(full_name);
            }
        }
    }
    noted
}

/// Put the report together: the counts, and a line for everything still
/// registered — tied back to its own removal when that removal was still
/// running when we stopped waiting.
///
/// Always a `Swept` report, because this is the end of a sweep that ran; the
/// other two reaches belong to sweeps that never got here and come from
/// [`PackageSweep::nothing_attempted`].
///
/// Pure, along with [`note_removals`], and between them they decide the whole
/// shape of the report — so the shape can be tested without a package to
/// remove, which on a developer's machine is the only honest way to test it.
fn gather(
    found: Vec<Registration>,
    removals: Removals,
    remaining: Vec<Registration>,
    mut trouble: Vec<String>,
) -> PackageSweep {
    for still in &remaining {
        trouble
            .extend(still.still_registered_lines(removals.still_going.contains(&still.full_name)));
    }
    PackageSweep {
        reach: PackageReach::Swept,
        found,
        removed: removals.removed,
        remaining,
        trouble,
    }
}

/// Walk a WinRT iterable by hand.
///
/// Neither adapter the `windows` crate offers will do here. `IntoIterator for
/// &IIterable<T>` is `self.First().unwrap()` — a panic, and as SYSTEM a panic
/// goes through this crate's hook and writes a `crash.log` into the system
/// profile, which is precisely the sort of leftover this mode exists to remove.
/// And `IIterator::next` swallows both `Current()` and `MoveNext()` errors, so
/// an enumeration that breaks halfway simply ends, and a list that was cut
/// short is indistinguishable from a machine that has less on it.
///
/// Here the first failure stops the walk and comes back in words for the caller
/// to place. What was visited before it is not lost: `visit` has already had it.
fn walk<T: RuntimeType + 'static>(
    items: &IIterable<T>,
    mut visit: impl FnMut(T),
) -> Result<(), String> {
    let walk = items
        .First()
        .map_err(|e| format!("it would not start ({e})"))?;
    while walk
        .HasCurrent()
        .map_err(|e| format!("it would not say whether there was more ({e})"))?
    {
        visit(
            walk.Current()
                .map_err(|e| format!("an entry would not be read ({e})"))?,
        );
        walk.MoveNext()
            .map_err(|e| format!("it would not step on ({e})"))?;
    }
    Ok(())
}

/// Every registration of our package, for every account. `when` says which pass
/// this is, so two failures do not read as one repeated.
///
/// `FindPackages` with no SID is every account's packages, which Windows allows
/// only to an administrator — SYSTEM is one.
///
/// `Err` is the one failure the caller must not read as "nothing is
/// registered": the call itself refused, so the machine has not been listed at
/// all. A walk that breaks partway is not that — what it did list is real and
/// worth removing — so it comes back as `Ok` with a line in `trouble` saying
/// the list was cut short.
fn find_registrations(
    pm: &PackageManager,
    when: &str,
    trouble: &mut Vec<String>,
) -> Result<Found, String> {
    let packages = pm
        .FindPackages()
        .map_err(|e| format!("the machine's packages could not be listed {when} ({e})"))?;
    let mut family: Option<String> = None;
    let mut full_names: Vec<String> = Vec::new();
    let walked = walk(&packages, |package| {
        // A package whose id will not read cannot be shown to be ours, and
        // saying so for every other vendor's package on the machine would bury
        // what this report is for.
        let Ok(id) = package.Id() else { return };
        if id.Name().map(|name| name != PACKAGE_NAME).unwrap_or(true) {
            return;
        }
        if family.is_none() {
            family = id.FamilyName().ok().map(|name| name.to_string());
        }
        match id.FullName() {
            // One package registered for several accounts comes back once per
            // account; the accounts are `FindUsers`'s to list, not this walk's.
            Ok(full_name) => {
                let full_name = full_name.to_string();
                if !full_names.contains(&full_name) {
                    full_names.push(full_name);
                }
            }
            Err(e) => trouble.push(format!(
                "a registration of {PACKAGE_NAME} would not give its full name {when} ({e})"
            )),
        }
    });
    if let Err(why) = walked {
        trouble.push(format!(
            "the machine's packages could not be walked to the end {when}: {why}"
        ));
    }
    // After the walk, not during it: no second call into the deployment service
    // while its own enumeration is still open.
    let registrations = full_names
        .into_iter()
        .map(|full_name| {
            let users = users_of(pm, &full_name, when, trouble);
            Registration { full_name, users }
        })
        .collect();
    Ok((registrations, family))
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
    let mut users = Vec::new();
    let walked = walk(&found, |user| {
        users.push(PackageUser {
            sid: match user.UserSecurityId() {
                Ok(sid) if !sid.is_empty() => sid.to_string(),
                _ => "an account the deployment service would not name".to_string(),
            },
            state: user
                .InstallState()
                .map(state_wording)
                .unwrap_or_else(|e| format!("its state would not read ({e})")),
        });
    });
    if let Err(why) = walked {
        trouble.push(format!(
            "{full_name}: the accounts it is registered for could not be walked to the end {when}: {why}"
        ));
    }
    users
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

/// Deprovision the package family, when the machine has one provisioned.
///
/// The family is taken from the provisioned list itself, matched by name, and
/// only falls back to `registered` — a family name the enumeration happened to
/// read — if the provisioned entry will not give one. That way round because a
/// package provisioned for all users but registered for nobody is exactly the
/// state a search led by registrations would walk straight past, and exactly
/// the state worth catching.
///
/// Nothing in Kuvatin's install provisions it — registration is per user, from
/// the app itself — so this finds nothing today and costs one call for the day
/// something changes. Microsoft's per-machine uninstall order is deprovision
/// first, remove second, and that is the order here.
fn deprovision_provisioned(
    pm: &PackageManager,
    registered: Option<&str>,
    trouble: &mut Vec<String>,
) {
    let provisioned = match pm.FindProvisionedPackages() {
        Ok(provisioned) => provisioned,
        Err(e) => {
            trouble.push(format!(
                "the machine's provisioned packages could not be listed ({e})"
            ));
            return;
        }
    };
    // An `IVector`, whose iterator is an index walk over `GetAt` — no `unwrap`
    // to panic on, unlike the `IIterable` adapter [`walk`] exists to avoid. It
    // does end silently if `GetAt` ever failed, and a list cut short reads here
    // as "nothing is provisioned": the sweep would deprovision nothing and say
    // nothing about it. Nothing in Kuvatin's install ever provisions the
    // package, so that is a risk on an empty list, and worth the four lines it
    // saves.
    let ours = provisioned.into_iter().find(|package| {
        package
            .Id()
            .and_then(|id| id.Name())
            .map(|name| name == PACKAGE_NAME)
            .unwrap_or(false)
    });
    let Some(ours) = ours else { return };
    let family = match ours.Id().and_then(|id| id.FamilyName()) {
        Ok(family) => family.to_string(),
        Err(e) => {
            let Some(registered) = registered else {
                trouble.push(format!(
                    "{PACKAGE_NAME} is provisioned for all users but would not give its family name ({e}), so it could not be deprovisioned"
                ));
                return;
            };
            trouble.push(format!(
                "{PACKAGE_NAME} is provisioned for all users but would not give its family name ({e}); going by the registered one"
            ));
            registered.to_string()
        }
    };
    let started = pm.DeprovisionPackageForAllUsersAsync(&HSTRING::from(family.as_str()));
    let why = match started {
        Ok(op) => match await_deployment("deprovisioning", &op) {
            Ok(result) => verdict("deprovisioning", &result).err(),
            Err(ended) => Some(ended.why().to_string()),
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
        Err(Unfinished::StillGoing(why)) => Removal::StillGoing(why),
        Err(Unfinished::Failed(why)) => Removal::Refused(why),
    }
}

/// Why a bounded wait ended without the work having finished.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Unfinished {
    /// The deadline passed and the operation was still running. Nothing has
    /// stopped: the deployment service is still doing it.
    StillGoing(String),
    /// The operation failed, or could not be asked how it was going.
    Failed(String),
}

impl Unfinished {
    fn why(&self) -> &str {
        match self {
            Unfinished::StillGoing(why) | Unfinished::Failed(why) => why,
        }
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
) -> Result<DeploymentResult, Unfinished> {
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
    op.GetResults()
        .map_err(|e| Unfinished::Failed(format!("{what} failed ({e})")))
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
) -> Result<(), Unfinished> {
    let deadline = Instant::now() + timeout;
    loop {
        if finished().map_err(Unfinished::Failed)? {
            return Ok(());
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Unfinished::StillGoing(format!(
                "{what} was still going after {timeout:?}; the uninstall waited no longer"
            )));
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
    use windows::Win32::Foundation::{E_OUTOFMEMORY, S_FALSE, S_OK};

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

    /// The driver's own shape, without the deployment service: the removals are
    /// noted where they happen, between the two enumerations, so that what
    /// comes back reads in the order it happened in. `before` and `after` are
    /// what those two passes had to say.
    fn sweep_of(
        found: Vec<Registration>,
        removals: Vec<(String, Removal)>,
        remaining: Vec<Registration>,
        before: &[&str],
        after: &[&str],
    ) -> PackageSweep {
        let mut trouble: Vec<String> = before.iter().map(|line| line.to_string()).collect();
        let removals = note_removals(removals, &mut trouble);
        trouble.extend(after.iter().map(|line| line.to_string()));
        gather(found, removals, remaining, trouble)
    }

    /// The whole report, over data that no package had to be removed to
    /// produce: what went is counted, what would not go is named, what outlived
    /// the removals is named once per account holding it, and all of it reads
    /// in the order it happened.
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
        let sweep = sweep_of(
            vec![gone.clone(), stuck.clone()],
            vec![
                (gone.full_name.clone(), Removal::Gone),
                (
                    stuck.full_name.clone(),
                    Removal::Refused("the service said no".to_string()),
                ),
            ],
            vec![stuck.clone()],
            &["the first pass had something to say"],
            &["the second pass had something to say"],
        );

        assert_eq!(sweep.removed, 1, "one of the two went");
        assert_eq!(sweep.found, vec![gone.clone(), stuck.clone()]);
        assert_eq!(sweep.remaining, vec![stuck.clone()]);
        // The refusal names the package that refused.
        assert!(
            sweep
                .trouble
                .iter()
                .any(|line| line.starts_with(&stuck.full_name)
                    && line.contains("the service said no")),
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
        // Chronological: the first pass, then the removals, then the second
        // pass, then what the second pass found still there.
        let at = |needle: &str| {
            sweep
                .trouble
                .iter()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("nothing about {needle}: {:?}", sweep.trouble))
        };
        assert!(
            at("the first pass") < at("the service said no"),
            "{:?}",
            sweep.trouble
        );
        assert!(
            at("the service said no") < at("the second pass"),
            "{:?}",
            sweep.trouble
        );
        assert!(
            at("the second pass") < at("still registered"),
            "{:?}",
            sweep.trouble
        );
    }

    #[test]
    fn a_sweep_that_removed_everything_has_nothing_to_report() {
        let one = registration(
            "VilleMattila.Kuvatin_2.9.1.0_x64__8wekyb3d8bbwe",
            &[user("S-1-5-21-1", "installed")],
        );
        let sweep = sweep_of(
            vec![one.clone()],
            vec![(one.full_name.clone(), Removal::Gone)],
            Vec::new(),
            &[],
            &[],
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
        let sweep = sweep_of(
            vec![orphan.clone()],
            vec![(orphan.full_name.clone(), Removal::Gone)],
            vec![orphan.clone()],
            &[],
            &[],
        );
        assert_eq!(sweep.trouble.len(), 1, "{:?}", sweep.trouble);
        assert!(
            sweep.trouble[0].contains(&orphan.full_name)
                && sweep.trouble[0].contains("still registered"),
            "{}",
            sweep.trouble[0]
        );
    }

    /// A removal that was still running when the deadline passed is not a
    /// removal that failed: the deployment service carries on with it after the
    /// uninstall has stopped waiting. So the leftover the second pass then
    /// finds says so, rather than reading as a package that would not go — and
    /// a leftover from a removal that really was refused does not.
    #[test]
    fn a_leftover_from_a_removal_still_running_says_it_may_yet_clear() {
        let slow = registration(
            "VilleMattila.Kuvatin_2.9.1.0_x64__8wekyb3d8bbwe",
            &[user("S-1-5-21-2", "installed")],
        );
        let refused = registration(
            "VilleMattila.Kuvatin_2.8.0.0_x64__8wekyb3d8bbwe",
            &[user("S-1-5-21-2", "staged")],
        );
        let sweep = sweep_of(
            vec![slow.clone(), refused.clone()],
            vec![
                (
                    slow.full_name.clone(),
                    Removal::StillGoing("the removal was still going after 180s".to_string()),
                ),
                (
                    refused.full_name.clone(),
                    Removal::Refused("the service said no".to_string()),
                ),
            ],
            vec![slow.clone(), refused.clone()],
            &[],
            &[],
        );
        assert_eq!(sweep.removed, 0, "neither was reported removed");
        let leftover = |full_name: &str| {
            sweep
                .trouble
                .iter()
                .find(|line| line.starts_with(full_name) && line.contains("still registered"))
                .unwrap_or_else(|| panic!("no leftover line for {full_name}: {:?}", sweep.trouble))
                .clone()
        };
        assert!(
            leftover(&slow.full_name).contains("still going"),
            "{}",
            leftover(&slow.full_name)
        );
        assert!(
            !leftover(&refused.full_name).contains("still going"),
            "{}",
            leftover(&refused.full_name)
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

    /// The deadline passing is its own kind of ending, told apart from a
    /// failure: the work carries on, and the report says so rather than calling
    /// it refused.
    #[test]
    fn a_bounded_wait_gives_up_and_says_what_was_going_and_for_how_long() {
        let looks = Cell::new(0);
        let ended = wait_for(
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
            matches!(ended, Unfinished::StillGoing(_)),
            "a deadline is not a failure: {ended:?}"
        );
        assert!(
            ended
                .why()
                .starts_with("the removal was still going after 40ms"),
            "{}",
            ended.why()
        );
        assert!(
            looks.get() > 1,
            "40ms of 5ms ticks should be more than one look: {}",
            looks.get()
        );
    }

    #[test]
    fn a_bounded_wait_hands_back_a_status_it_could_not_read() {
        let ended = wait_for(
            "the removal",
            Duration::from_secs(60),
            Duration::from_millis(1),
            || Err("its status would not read".to_string()),
        )
        .expect_err("an unreadable status ends the wait");
        assert_eq!(
            ended,
            Unfinished::Failed("its status would not read".to_string())
        );
    }

    /// Only a COM that would not start at all is worth a word. The rest are
    /// what an ordinary process meets: already in an apartment, or already in
    /// an STA — which the polled wait does not mind.
    #[test]
    fn only_a_com_that_would_not_start_is_worth_mentioning() {
        assert_eq!(apartment_aside(S_OK), "");
        assert_eq!(apartment_aside(S_FALSE), "");
        assert_eq!(apartment_aside(RPC_E_CHANGED_MODE), "");
        let said = apartment_aside(E_OUTOFMEMORY);
        assert!(said.contains("8007000E"), "{said}");
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
        // A deployment service that will not start is a shortcoming of the
        // machine, not of the code under test — and on CI it is a failure,
        // because there the service is there to be started.
        let pm = match PackageManager::new() {
            Ok(pm) => pm,
            Err(e) => {
                skip_or_fail_on_ci(&format!("the deployment service would not start ({e})"));
                return;
            }
        };
        let mut trouble = Vec::new();
        let (found, family) = find_registrations(&pm, "in a test", &mut trouble)
            .expect("an elevated FindPackages lists every account's packages");
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
