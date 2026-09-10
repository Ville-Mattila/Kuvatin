//! Kuvatin's Windows 11 context-menu handler.
//!
//! Windows 11 shows only packaged `IExplorerCommand` handlers in its top-level
//! context menu; everything else lands under "Show more options". This DLL is
//! that handler: a "Kuvatin" entry whose submenu lists the user's presets and
//! the fixed actions (the same list the classic registry menu shows, from
//! `kuvatin_core::menu`), each of which launches `kuvatin.exe` next to this
//! DLL with the whole selection on one command line.
//!
//! The shell loads it through the sparse package `crates/kuvatin/msix`
//! registers (a `com:SurrogateServer` class), so it runs in a `dllhost.exe`
//! surrogate, never inside Explorer itself. It has no GStreamer dependency.

#![cfg(windows)]

mod command;

use std::ffi::c_void;
use windows::core::{implement, IUnknown, Interface, GUID, HRESULT};
use windows::Win32::Foundation::{BOOL, CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, S_FALSE};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};

/// The handler's CLSID. Must match `Clsid` / `com:Class Id` in
/// `crates/kuvatin/msix/AppxManifest.xml` (via build-msix.ps1).
pub const CLSID_KUVATIN_MENU: GUID = GUID::from_u128(0x7a3e2b6c_9d14_4f58_8b2a_1c6e5d4f3a90);

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Option<&IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        if punkouter.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let root: IUnknown = command::RootCommand::new().into();
        unsafe { root.query(riid, ppvobject).ok() }
    }

    fn LockServer(&self, _flock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

/// COM entry point: hand out the class factory for our one CLSID.
///
/// # Safety
/// Called by COM with valid pointers: `rclsid` and `riid` point at GUIDs and
/// `ppv` at an out-pointer, per the DllGetClassObject contract.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if rclsid.is_null() || *rclsid != CLSID_KUVATIN_MENU {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory: IClassFactory = Factory.into();
    factory.query(riid, ppv)
}

/// The surrogate host owns our lifetime; never ask to be unloaded early.
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}
