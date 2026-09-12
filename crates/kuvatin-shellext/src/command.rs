//! The `IExplorerCommand` objects: one root ("Kuvatin", has subcommands), one
//! per submenu item, and the enumerator that hands the items to the shell.

use kuvatin_core::menu::{action_args, menu_items, Action, MenuItem};
use kuvatin_core::preset::PresetStore;
use std::cell::{Cell, OnceCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use windows::core::{implement, Error, Result, GUID, HSTRING, PWSTR};
use windows::Win32::Foundation::{BOOL, E_FAIL, E_INVALIDARG, E_NOTIMPL, S_FALSE, S_OK};
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
///
/// Deliberately `load`, not `load_or_init`: the latter creates a missing file,
/// copies a corrupt one aside and persists migrations. Opening a right-click
/// menu must not write to the user's config, least of all from inside the
/// shell's surrogate process.
fn current_items() -> Vec<MenuItem> {
    let store = PresetStore::default_path()
        .map(|p| PresetStore::load(&p))
        .unwrap_or_else(PresetStore::builtin);
    menu_items(&store)
}

/// Run a COM method body with panics contained.
///
/// These functions are called across an `extern "system"` boundary, where an
/// unwinding panic aborts the process — and that process is the shell's
/// surrogate, so the whole Kuvatin menu would vanish until the shell restarts.
/// Parsing the preset store is the realistic source.
fn guard<T>(what: &'static str, body: impl FnOnce() -> Result<T>) -> Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(result) => result,
        Err(_) => Err(Error::new(
            E_FAIL,
            format!("Kuvatin's context menu failed in {what}"),
        )),
    }
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
        // No stable identity to offer; a zero identifier claimed the same one
        // for the root and every item alike.
        Err(E_NOTIMPL.into())
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
        guard("building the submenu", || {
            // One shared answer to "is this selection all frames?", filled in
            // by whichever item the shell asks about first.
            let frames_only: Rc<OnceCell<bool>> = Rc::new(OnceCell::new());
            let items: Vec<IExplorerCommand> = current_items()
                .into_iter()
                .map(|item| {
                    ItemCommand {
                        item,
                        frames_only: frames_only.clone(),
                    }
                    .into()
                })
                .collect();
            Ok(ItemEnum {
                items,
                pos: Cell::new(0),
            }
            .into())
        })
    }
}

/// One submenu item: a preset, the sequence render, or "Open in Kuvatin…".
#[implement(IExplorerCommand)]
struct ItemCommand {
    item: MenuItem,
    /// Whether the selection is entirely sequence-only frames, decided once
    /// per menu and shared by every item. The shell asks each item for its
    /// state in turn, and re-reading a large selection for each one meant
    /// thousands of shell round trips before the menu could draw.
    frames_only: Rc<OnceCell<bool>>,
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
        // No stable identity to offer; a zero identifier claimed the same one
        // for the root and every item alike.
        Err(E_NOTIMPL.into())
    }

    fn GetState(&self, items: Option<&IShellItemArray>, _ok_to_be_slow: BOOL) -> Result<u32> {
        guard("deciding a menu item's state", || {
            // Presets can't read sequence-only frames; hide them for an all-EXR
            // selection so the submenu is just the sequence render (+ open).
            if !matches!(self.item.action, Action::Preset(_)) {
                return Ok(ECS_ENABLED.0 as u32);
            }
            let hide = *self
                .frames_only
                .get_or_init(|| frames_only(&selected_paths(items)));
            Ok(if hide { ECS_HIDDEN.0 } else { ECS_ENABLED.0 } as u32)
        })
    }

    fn Invoke(&self, items: Option<&IShellItemArray>, _bc: Option<&IBindCtx>) -> Result<()> {
        guard("running a menu item", || {
            let paths = selected_paths(items);
            if paths.is_empty() {
                return Ok(());
            }
            // E_FAIL, not E_NOTIMPL: the shell reads "not implemented" as
            // "there is no such command" and says nothing, so a failure to
            // start looked exactly like a menu item that does nothing.
            let Some(exe) = exe_path() else {
                return Err(Error::new(
                    E_FAIL,
                    "kuvatin.exe is not next to the context-menu handler",
                ));
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
                .map_err(|e| Error::new(E_FAIL, format!("could not start Kuvatin: {e}")))
        })
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
        if !pceltfetched.is_null() {
            unsafe {
                *pceltfetched = 0;
            }
        }
        // The shell owns this array and it may be uninitialised. Asking for
        // several without somewhere to report the count is equally unusable.
        if puicommand.is_null() || (celt > 1 && pceltfetched.is_null()) {
            return E_INVALIDARG;
        }
        let mut fetched = 0u32;
        let mut pos = self.pos.get();
        while fetched < celt && pos < self.items.len() {
            unsafe {
                // write, not assign: assigning drops whatever Rust believes is
                // already at that address, which would release a junk pointer.
                std::ptr::write(
                    puicommand.add(fetched as usize),
                    Some(self.items[pos].clone()),
                );
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

#[cfg(test)]
mod tests {
    use super::*;
    use kuvatin_core::menu::{Action, MenuItem};

    fn item(n: usize) -> MenuItem {
        MenuItem {
            id: format!("Kuvatin.{n:02}"),
            label: format!("Item {n}"),
            action: Action::Gui,
            separator_before: false,
        }
    }

    fn enumerator(count: usize) -> IEnumExplorerCommand {
        let items: Vec<IExplorerCommand> = (0..count)
            .map(|n| {
                let cmd: IExplorerCommand = ItemCommand {
                    item: item(n),
                    frames_only: Default::default(),
                }
                .into();
                cmd
            })
            .collect();
        ItemEnum {
            items,
            pos: Cell::new(0),
        }
        .into()
    }

    fn next_one(
        e: &IEnumExplorerCommand,
    ) -> (windows::core::HRESULT, u32, Option<IExplorerCommand>) {
        let mut out: [Option<IExplorerCommand>; 1] = Default::default();
        let mut fetched = 0u32;
        let hr = unsafe { e.Next(&mut out, Some(&mut fetched)) };
        (hr, fetched, out[0].take())
    }

    /// The shell walks the submenu one command at a time and stops on S_FALSE;
    /// getting that wrong either truncates the menu or spins.
    #[test]
    fn the_enumerator_hands_out_every_item_then_stops() {
        let e = enumerator(3);
        let mut titles = Vec::new();
        for _ in 0..3 {
            let (hr, fetched, cmd) = next_one(&e);
            assert_eq!(hr, S_OK);
            assert_eq!(fetched, 1);
            let cmd = cmd.expect("a command");
            let title = unsafe { cmd.GetTitle(None) }.expect("title");
            titles.push(unsafe { title.to_string() }.unwrap());
        }
        assert_eq!(titles, ["Item 0", "Item 1", "Item 2"]);

        let (hr, fetched, cmd) = next_one(&e);
        assert_eq!(hr, S_FALSE, "exhausted");
        assert_eq!(fetched, 0);
        assert!(cmd.is_none());
    }

    /// An empty submenu must report exhaustion immediately rather than hand
    /// back a command that does not exist.
    #[test]
    fn an_empty_enumerator_is_exhausted_at_once() {
        let (hr, fetched, cmd) = next_one(&enumerator(0));
        assert_eq!(hr, S_FALSE);
        assert_eq!(fetched, 0);
        assert!(cmd.is_none());
    }

    /// Asking for several at once fills what it can and reports how many.
    #[test]
    fn a_batched_request_reports_what_it_filled() {
        let e = enumerator(2);
        let mut out: [Option<IExplorerCommand>; 4] = Default::default();
        let mut fetched = 0u32;
        let hr = unsafe { e.Next(&mut out, Some(&mut fetched)) };
        assert_eq!(hr, S_FALSE, "fewer than asked");
        assert_eq!(fetched, 2);
        assert!(out[0].is_some() && out[1].is_some());
        assert!(out[2].is_none() && out[3].is_none(), "untouched slots");
    }

    /// A null output pointer is a caller error, not a crash. The safe wrapper
    /// cannot express one, so this goes through the vtable the shell uses.
    #[test]
    fn a_null_destination_is_rejected() {
        use windows::core::Interface;
        let e = enumerator(2);
        let mut fetched = 0u32;
        let hr = unsafe {
            (Interface::vtable(&e).Next)(
                Interface::as_raw(&e),
                1,
                std::ptr::null_mut(),
                &mut fetched,
            )
        };
        assert_eq!(hr, windows::Win32::Foundation::E_INVALIDARG);
        assert_eq!(fetched, 0);
    }

    #[test]
    fn skip_clamps_and_reset_restarts() {
        let e = enumerator(3);
        unsafe { e.Skip(2) }.expect("skip");
        assert_eq!(next_one(&e).1, 1, "one left after skipping two");
        unsafe { e.Skip(99) }.expect("skip past the end");
        assert_eq!(next_one(&e).0, S_FALSE);
        unsafe { e.Reset() }.expect("reset");
        assert_eq!(next_one(&e).1, 1, "enumeration restarts");
    }

    /// A clone carries the current position and then moves independently.
    #[test]
    fn a_clone_starts_where_the_original_stands() {
        let e = enumerator(3);
        assert_eq!(next_one(&e).1, 1);
        let c = unsafe { e.Clone() }.expect("clone");
        assert_eq!(next_one(&c).1, 1);
        assert_eq!(next_one(&c).1, 1);
        assert_eq!(next_one(&c).0, S_FALSE, "clone exhausted");
        assert_eq!(next_one(&e).1, 1, "original still has items");
    }

    /// Presets cannot read sequence-only frames, so they hide for a selection
    /// made entirely of them; anything else keeps the full submenu.
    #[test]
    fn frames_only_recognises_a_sequence_selection() {
        let p = |s: &str| PathBuf::from(s);
        assert!(frames_only(&[p("a.exr"), p("b.EXR")]), "case is ignored");
        assert!(!frames_only(&[p("a.exr"), p("b.png")]), "mixed selection");
        assert!(!frames_only(&[p("a.png")]));
        assert!(!frames_only(&[]), "an empty selection hides nothing");
        assert!(!frames_only(&[p("noextension")]));
    }
}
