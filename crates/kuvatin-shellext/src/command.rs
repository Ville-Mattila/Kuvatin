//! The `IExplorerCommand` objects: one root ("Kuvatin", has subcommands), one
//! per submenu item, and the enumerator that hands the items to the shell.

use kuvatin_core::menu::{action_args, menu_items, Action, MenuItem};
use kuvatin_core::preset::PresetStore;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use windows::core::{implement, Result, GUID, HSTRING, PWSTR};
use windows::Win32::Foundation::{BOOL, E_NOTIMPL, S_FALSE, S_OK};
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows::Win32::UI::Shell::{
    IEnumExplorerCommand, IEnumExplorerCommand_Impl, IExplorerCommand, IExplorerCommand_Impl,
    IShellItemArray, SHStrDupW, ECF_DEFAULT, ECF_HASSUBCOMMANDS, ECS_ENABLED, ECS_HIDDEN,
    SIGDN_FILESYSPATH,
};

/// Frame formats the image presets can't read (mirrors the registry menu's
/// sequence-only store for `.exr`).
const FRAME_ONLY_EXTENSIONS: &[&str] = &["exr"];

/// The directory this DLL was loaded from: the package's external location,
/// where `kuvatin.exe` lives too.
fn install_dir() -> Option<PathBuf> {
    unsafe {
        let mut module = Default::default();
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            windows::core::PCWSTR(install_dir as *const () as *const u16),
            &mut module,
        )
        .ok()?;
        let mut buf = [0u16; 32768];
        let n = GetModuleFileNameW(module, &mut buf) as usize;
        if n == 0 || n >= buf.len() {
            return None;
        }
        let path = PathBuf::from(String::from_utf16_lossy(&buf[..n]));
        path.parent().map(Path::to_path_buf)
    }
}

fn exe_path() -> Option<PathBuf> {
    install_dir().map(|d| d.join("kuvatin.exe"))
}

fn com_string(s: &str) -> Result<PWSTR> {
    unsafe { SHStrDupW(&HSTRING::from(s)) }
}

/// The selected items as file-system paths (folders included).
fn selected_paths(items: Option<&IShellItemArray>) -> Vec<PathBuf> {
    let Some(items) = items else {
        return Vec::new();
    };
    let mut out = Vec::new();
    unsafe {
        let count = items.GetCount().unwrap_or(0);
        for i in 0..count {
            if let Ok(item) = items.GetItemAt(i) {
                if let Ok(name) = item.GetDisplayName(SIGDN_FILESYSPATH) {
                    out.push(PathBuf::from(name.to_string().unwrap_or_default()));
                    windows::Win32::System::Com::CoTaskMemFree(Some(name.0 as *const _));
                }
            }
        }
    }
    out
}

/// True when every selected file is a sequence-only frame (`.exr`): the
/// preset items are hidden then, exactly like the registry menu's frame store.
fn frames_only(paths: &[PathBuf]) -> bool {
    !paths.is_empty()
        && paths.iter().all(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| FRAME_ONLY_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
        })
}

/// The user's presets, or the built-ins if the store can't be read — read at
/// menu time, so a preset saved in the GUI shows up on the next right-click.
fn current_items() -> Vec<MenuItem> {
    let store = PresetStore::default_path()
        .and_then(|p| PresetStore::load_or_init(&p).ok())
        .unwrap_or_else(PresetStore::builtin);
    menu_items(&store)
}

// ---------------------------------------------------------------------------

/// The top-level "Kuvatin" entry.
#[implement(IExplorerCommand)]
pub struct RootCommand;

impl RootCommand {
    pub fn new() -> Self {
        RootCommand
    }
}

impl IExplorerCommand_Impl for RootCommand_Impl {
    fn GetTitle(&self, _items: Option<&IShellItemArray>) -> Result<PWSTR> {
        com_string("Kuvatin")
    }

    fn GetIcon(&self, _items: Option<&IShellItemArray>) -> Result<PWSTR> {
        match exe_path() {
            Some(exe) => com_string(&format!("{},0", exe.display())),
            None => Err(E_NOTIMPL.into()),
        }
    }

    fn GetToolTip(&self, _items: Option<&IShellItemArray>) -> Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetCanonicalName(&self) -> Result<GUID> {
        Ok(GUID::zeroed())
    }

    fn GetState(&self, _items: Option<&IShellItemArray>, _ok_to_be_slow: BOOL) -> Result<u32> {
        // The package manifest already scopes the verb to our file types,
        // folders and folder backgrounds.
        Ok(ECS_ENABLED.0 as u32)
    }

    fn Invoke(&self, _items: Option<&IShellItemArray>, _bc: Option<&IBindCtx>) -> Result<()> {
        // Never invoked directly: the shell expands the subcommands instead.
        Ok(())
    }

    fn GetFlags(&self) -> Result<u32> {
        Ok(ECF_HASSUBCOMMANDS.0 as u32)
    }

    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        let items: Vec<IExplorerCommand> = current_items()
            .into_iter()
            .map(|item| ItemCommand { item }.into())
            .collect();
        Ok(ItemEnum {
            items,
            pos: Cell::new(0),
        }
        .into())
    }
}

/// One submenu item: a preset, the sequence render, or "Open in Kuvatin…".
#[implement(IExplorerCommand)]
struct ItemCommand {
    item: MenuItem,
}

impl IExplorerCommand_Impl for ItemCommand_Impl {
    fn GetTitle(&self, _items: Option<&IShellItemArray>) -> Result<PWSTR> {
        com_string(&self.item.label)
    }

    fn GetIcon(&self, _items: Option<&IShellItemArray>) -> Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetToolTip(&self, _items: Option<&IShellItemArray>) -> Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetCanonicalName(&self) -> Result<GUID> {
        Ok(GUID::zeroed())
    }

    fn GetState(&self, items: Option<&IShellItemArray>, _ok_to_be_slow: BOOL) -> Result<u32> {
        // Presets can't read sequence-only frames; hide them for an all-EXR
        // selection so the submenu is just the sequence render (+ open).
        let hide =
            matches!(self.item.action, Action::Preset(_)) && frames_only(&selected_paths(items));
        Ok(if hide { ECS_HIDDEN.0 } else { ECS_ENABLED.0 } as u32)
    }

    fn Invoke(&self, items: Option<&IShellItemArray>, _bc: Option<&IBindCtx>) -> Result<()> {
        let paths = selected_paths(items);
        if paths.is_empty() {
            return Ok(());
        }
        let Some(exe) = exe_path() else {
            return Err(E_NOTIMPL.into());
        };
        // One process for the whole selection (the classic menu launches one
        // per item and folds them at runtime; here the shell hands us all).
        let mut cmd = std::process::Command::new(exe);
        cmd.args(action_args(&self.item.action)).args(&paths);
        if let Some(dir) = install_dir() {
            cmd.current_dir(dir);
        }
        cmd.spawn()
            .map(drop)
            .map_err(|e| windows::core::Error::new(E_NOTIMPL, e.to_string()))
    }

    fn GetFlags(&self) -> Result<u32> {
        Ok(ECF_DEFAULT.0 as u32)
    }

    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        Err(E_NOTIMPL.into())
    }
}

/// Hands the submenu items to the shell, in order.
#[implement(IEnumExplorerCommand)]
struct ItemEnum {
    items: Vec<IExplorerCommand>,
    pos: Cell<usize>,
}

impl IEnumExplorerCommand_Impl for ItemEnum_Impl {
    fn Next(
        &self,
        celt: u32,
        puicommand: *mut Option<IExplorerCommand>,
        pceltfetched: *mut u32,
    ) -> windows::core::HRESULT {
        let mut fetched = 0u32;
        let mut pos = self.pos.get();
        while fetched < celt && pos < self.items.len() {
            unsafe {
                *puicommand.add(fetched as usize) = Some(self.items[pos].clone());
            }
            fetched += 1;
            pos += 1;
        }
        self.pos.set(pos);
        if !pceltfetched.is_null() {
            unsafe {
                *pceltfetched = fetched;
            }
        }
        if fetched == celt {
            S_OK
        } else {
            S_FALSE
        }
    }

    fn Skip(&self, celt: u32) -> Result<()> {
        self.pos
            .set((self.pos.get() + celt as usize).min(self.items.len()));
        Ok(())
    }

    fn Reset(&self) -> Result<()> {
        self.pos.set(0);
        Ok(())
    }

    fn Clone(&self) -> Result<IEnumExplorerCommand> {
        Ok(ItemEnum {
            items: self.items.clone(),
            pos: Cell::new(self.pos.get()),
        }
        .into())
    }
}
