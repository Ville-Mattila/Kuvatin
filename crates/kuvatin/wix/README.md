# Building the Kuvatin Windows installer (MSI)

The `.msi` installer is produced with [`cargo-wix`](https://github.com/volks73/cargo-wix),
which drives the **WiX Toolset v3** (`candle.exe` / `light.exe`). On install the
installer runs `kuvatin.exe --register` to add the Explorer context-menu entries;
on uninstall it runs `kuvatin.exe --unregister` to remove them (see the deferred
custom actions in `main.wxs`).

## Prerequisites

1. **cargo-wix**

   ```pwsh
   cargo install cargo-wix
   ```

2. **WiX Toolset v3** (provides `candle.exe` and `light.exe`).

   The winget package (`WiXToolset.WiXToolset`) requires administrator
   privileges and the .NET 3.5 (`NetFx3`) Windows Feature, which may not be
   available in every environment. A no-admin alternative is to download the
   official **binaries zip** from the WiX v3 releases and unzip it — the
   `candle`/`light` executables run on the already-present .NET 4.x runtime and
   need no installer:

   ```pwsh
   $dest = "$env:USERPROFILE\wix3-bin"
   Invoke-WebRequest `
     -Uri "https://github.com/wixtoolset/wix3/releases/download/wix3141rtm/wix314-binaries.zip" `
     -OutFile "$env:TEMP\wix314-binaries.zip"
   Expand-Archive "$env:TEMP\wix314-binaries.zip" -DestinationPath $dest -Force
   $env:Path = "$dest;$env:Path"   # so cargo-wix finds candle.exe / light.exe
   ```

   cargo-wix locates the toolset via the `WIX` environment variable, then falls
   back to `PATH`. With the zip approach above, having the bin folder on `PATH`
   is sufficient. You can also point at it explicitly with `cargo wix -b <bin>`.

## Build

This is a Cargo **workspace**, so the package must be selected with `-p`. The
WiX source references its sidecar files (e.g. `License.rtf`) with paths relative
to the `wix/` folder, so run the command **from the package directory** so those
relative paths resolve.

The app links GStreamer and bundles a **trimmed** subset of its runtime into
the installer, so the build is three steps: build the release exe (the bundle
script walks its PE imports to compute which DLLs are needed), harvest the
runtime (an allow-list of plugins, their DLL closure, and every component's
license text — needs the GStreamer SDK installed and `heat.exe` on PATH), then
build with the staging path passed to the compiler. Pass `-AllPlugins` to the
script to bundle the entire distribution instead (the pre-2.7 behaviour).

```pwsh
# 0. The release exe seeds the DLL closure.
cargo build --release -p kuvatin

# 1. Stage the trimmed GStreamer runtime + licenses; generate wix/gstreamer.wxs (gitignored).
crates\kuvatin\wix\bundle-gstreamer.ps1 -StageDir "$PWD\target\gst-staging"

# 2. Build the Windows 11 menu package. main.wxs references MsixPath
#    unconditionally, so this is not optional.
cargo build --release -p kuvatin-shellext
crates\kuvatin\msix\build-msix.ps1 -Version 0.0.0 -Out target\msix\Kuvatin.msix

# 3. Build the MSI, pointing the compiler at both.
cd crates/kuvatin
cargo wix -p kuvatin --nocapture `
  --compiler-arg "-dGstStageDir=$(Resolve-Path ..\..\target\gst-staging)" `
  --compiler-arg "-dMsixPath=$(Resolve-Path ..\..\target\msix\Kuvatin.msix)"
```

Add `-dSignCerPath=<cer>` and `-dSignCerThumbprint=<thumb>` to build the signed
variant, which also installs the certificate and trusts it machine-wide.

`main.wxs` references the harvested `GstRuntime` component group, so step 1 must
run before step 2 (CI does both — see `.github/workflows/release.yml`).

The installer is written to `target/wix/kuvatin-<version>-x86_64.msi`
(e.g. `kuvatin-1.5.0-x86_64.msi`), ~33 MB (it was ~106 MB before the runtime was trimmed to what the app loads) because it carries the GStreamer
runtime. It is under `target/`, which is gitignored and not committed.

## Registration scope (per-machine MSI, per-user context menu)

The MSI installs **per-machine** (`InstallScope='perMachine'`), but Explorer
context-menu registration necessarily lives in **HKCU** (per user). The custom
actions run `--register`/`--unregister` impersonated as the installing user, so
out of the box only that user gets the menu. Two mitigations keep this sane:

- **Self-healing at launch:** the release GUI calls `shell::ensure_registered()`
  on every startup — two registry reads that re-run the full registration
  only when nothing owns the menu, the registered exe no longer exists (an
  upgrade moved the install directory) or the registered key set is older
  than this build's. Any user who launches the app once gets (and keeps) the
  context menu. It never hijacks: a menu owned by another Kuvatin that still
  exists is left alone, and debug builds never touch the registry at all —
  run `--register` explicitly from the copy that should own the menu.
- **Uninstall cleans every account:** the impersonated `--unregister` cleans
  the uninstalling user only; a second custom action, `kuvatin.exe
  --unregister-all-users`, then runs as SYSTEM (deferred, `Impersonate='no'`,
  `Return='ignore'`) after it and before `KuvatinUntrustCert`. It removes the
  classic verbs, the sparse package and the per-user files (logs,
  `%TEMP%\kuvatin`, the package's `AppData\Local\Packages` folder) for
  **every** profile on the machine — loading a signed-out account's
  `UsrClass.dat` when its hive is not already mounted. Presets and settings
  (`%APPDATA%\Kuvatin`) are kept. Registry keys are deleted through handles
  that never follow a symbolic link, and files are deleted by a walk from the
  vetted profile root that never follows a junction, so a hostile account
  cannot redirect the SYSTEM delete elsewhere. It is skipped during a major
  upgrade (`NOT UPGRADINGPRODUCTCODE`) so other users' menus survive an
  upgrade; `ensure_registered()` re-heals them per user at next launch. Its
  report — what ran, what it skipped and why — goes to stdout and lands in
  the verbose MSI log (`msiexec /x ... /l*v uninstall.log`) under the
  `KuvatinUnregisterAllUsers` action, every line prefixed `Kuvatin:`.

## Regenerating main.wxs

`main.wxs` was generated with `cargo wix init --force -p kuvatin` and then
hand-edited to add the `KuvatinRegister` / `KuvatinUnregister` deferred custom
actions and their `InstallExecuteSequence`. If you regenerate it, re-apply those
customizations. The custom actions reference the main executable by its `File`
`Id` (`exe0`) via `FileKey`.
