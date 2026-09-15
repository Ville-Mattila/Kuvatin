//! Test-only helpers shared by the shell modules' unit tests.
//!
//! What lives here is the awkward machinery for making the registry say no to
//! us on purpose. Every module in this uninstall path has to behave well when
//! a key will not open — `regutil::enum_subkeys` must not read a denied key as
//! an empty one, `verbs::subkeys_to_delete` must still hand back the static
//! list — and the only honest way to test that is to put a Deny ACE on a
//! scratch key we own and try. The recipe is fiddly enough (a security
//! descriptor, an ACL with the deny ACE ahead of the allow, and putting it all
//! back afterwards) that one copy is plenty.

use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE};
use windows::Win32::Security::{
    AddAccessAllowedAce, AddAccessDeniedAce, GetLengthSid, GetTokenInformation, InitializeAcl,
    InitializeSecurityDescriptor, SetSecurityDescriptorDacl, TokenUser, ACL, ACL_REVISION,
    DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_DESCRIPTOR, TOKEN_QUERY,
    TOKEN_USER,
};
use windows::Win32::System::Registry::{
    RegSetKeySecurity, HKEY_CURRENT_USER, KEY_ALL_ACCESS, REG_SAM_FLAGS,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::regutil::open_owned_no_links;

/// `WRITE_DAC`, needed to put a DACL on a key we own.
const WRITE_DAC_ACCESS: REG_SAM_FLAGS = REG_SAM_FLAGS(0x0004_0000);

/// This process's user SID, copied out of its token so it outlives the
/// buffer the token handed us.
fn my_sid() -> Result<Vec<u8>, String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|e| format!("OpenProcessToken: {e}"))?;
        let mut len = 0u32;
        // First call just sizes the buffer; it is expected to fail.
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        let got = GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        );
        let _ = CloseHandle(token);
        got.map_err(|e| format!("GetTokenInformation(TokenUser): {e}"))?;
        let user = &*buf.as_ptr().cast::<TOKEN_USER>();
        let n = GetLengthSid(user.User.Sid) as usize;
        Ok(std::slice::from_raw_parts(user.User.Sid.0.cast::<u8>(), n).to_vec())
    }
}

/// Put a DACL on `path` that denies this user `denied` and grants the rest.
/// `denied == 0` restores a NULL DACL, which grants everyone everything.
fn set_dacl(path: &str, denied: u32) -> Result<(), String> {
    let sid_bytes = my_sid()?;
    let mut acl_buf = vec![0u8; 1024];
    let mut sd = SECURITY_DESCRIPTOR::default();
    let psd = PSECURITY_DESCRIPTOR(std::ptr::addr_of_mut!(sd).cast());
    unsafe {
        // SECURITY_DESCRIPTOR_REVISION is 1.
        InitializeSecurityDescriptor(psd, 1).map_err(|e| format!("init sd: {e}"))?;
        if denied == 0 {
            SetSecurityDescriptorDacl(psd, true, None, false)
                .map_err(|e| format!("null dacl: {e}"))?;
        } else {
            let sid = PSID(sid_bytes.as_ptr() as *mut _);
            let acl = acl_buf.as_mut_ptr().cast::<ACL>();
            InitializeAcl(acl, acl_buf.len() as u32, ACL_REVISION)
                .map_err(|e| format!("init acl: {e}"))?;
            // Deny first: within a DACL the order is what decides.
            AddAccessDeniedAce(acl, ACL_REVISION, denied, sid)
                .map_err(|e| format!("deny ace: {e}"))?;
            AddAccessAllowedAce(acl, ACL_REVISION, KEY_ALL_ACCESS.0, sid)
                .map_err(|e| format!("allow ace: {e}"))?;
            SetSecurityDescriptorDacl(psd, true, Some(acl), false)
                .map_err(|e| format!("set dacl: {e}"))?;
        }
    }
    let key = open_owned_no_links(HKEY_CURRENT_USER, path, WRITE_DAC_ACCESS)?;
    let status = unsafe { RegSetKeySecurity(key.get(), DACL_SECURITY_INFORMATION, psd) };
    drop(key);
    if status != ERROR_SUCCESS {
        return Err(format!("RegSetKeySecurity: error {}", status.0));
    }
    Ok(())
}

/// Say that a test is skipping, and make that a failure where skipping would
/// be a lie.
///
/// A test that cannot build the thing it is testing — no junction to be made,
/// no Deny ACE to be set — proves nothing either way, and on a developer's
/// machine saying so and moving on is the right answer. On CI it is not: those
/// gates exist to run exactly these, and a run that quietly ran none of them
/// would be green and worth nothing. `CI` is set by every hosted runner,
/// GitHub Actions included.
///
/// Every self-skipping test in this crate goes through here or through
/// [`skip_even_on_ci`], so the rule is one rule and lives in one place.
///
/// What the release workflow relies on is the exit code, not the text: the
/// panic below is what turns a skipped gate into a failed build, and no step
/// greps the output for it. The line is `println!` rather than `eprintln!` all
/// the same, so that a developer running with `--nocapture` sees it in order
/// with the test names rather than interleaved from another stream.
#[track_caller]
pub(super) fn skip_or_fail_on_ci(reason: &str) {
    assert!(!on_ci(), "this test must not skip on CI: {reason}");
    println!("skipping: {reason}");
}

/// Whether this is a CI run: `CI` set to something that is not empty and not
/// `false`.
///
/// Presence alone will not do. Some local tooling exports `CI=false` to say the
/// opposite of what `var_os(..).is_some()` would read it as, and a developer
/// whose shell does that would find every one of these tests failing for a
/// reason nothing on screen explains.
fn on_ci() -> bool {
    match std::env::var("CI") {
        Ok(value) => !value.is_empty() && !value.eq_ignore_ascii_case("false"),
        Err(_) => false,
    }
}

/// The same line, for the one kind of skip that is honest everywhere: the
/// setup worked, and what the test then found is that the condition it needs
/// cannot hold *in this process*.
///
/// That is a fact about the process, not a shortcoming of the machine, so no
/// runner could do anything about it and failing there would be noise. Use this
/// only where the test has *measured* that the condition does not hold — never
/// where it merely failed to arrange it.
#[track_caller]
pub(super) fn skip_even_on_ci(reason: &str) {
    println!("skipping: {reason}");
}

/// Holds a Deny ACE on an `HKEY_CURRENT_USER` key and takes it off again, so a
/// scratch cleanup can still delete that key however the test ends.
///
/// `Err` when the DACL could not be set at all: a test that cannot deny itself
/// access proves nothing either way and should say it is skipping — through
/// [`skip_or_fail_on_ci`], since not being able to set a DACL is a shortcoming
/// of the environment — rather than pass in silence.
///
/// There is no second failure mode here, the one files have: a registry ACE
/// that is set always bites. `SeBackupPrivilege` lifts a DACL only for a handle
/// that asks for it, and nothing in this crate passes `REG_OPTION_BACKUP_RESTORE`.
pub(super) struct Denied {
    path: String,
}

impl Denied {
    pub(super) fn on(path: &str, right: REG_SAM_FLAGS) -> Result<Self, String> {
        set_dacl(path, right.0)?;
        Ok(Denied {
            path: path.to_string(),
        })
    }
}

impl Drop for Denied {
    fn drop(&mut self) {
        // We own the key, so WRITE_DAC is always ours to take back.
        if let Err(why) = set_dacl(&self.path, 0) {
            eprintln!("could not restore the DACL on {}: {why}", self.path);
        }
    }
}
