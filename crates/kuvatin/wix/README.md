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

The app links GStreamer and bundles its **runtime** into the installer, so the
build is two steps: harvest the runtime (needs the GStreamer SDK installed and
`heat.exe` on PATH), then build with the staging path passed to the compiler.

```pwsh
# 1. Stage the GStreamer runtime + generate wix/gstreamer.wxs (gitignored).
crates\kuvatin\wix\bundle-gstreamer.ps1 -StageDir "$PWD\target\gst-staging"

# 2. Build the MSI, pointing the compiler at the staged runtime.
cd crates/kuvatin
cargo wix -p kuvatin --nocapture --compiler-arg "-dGstStageDir=$(Resolve-Path ..\..\target\gst-staging)"
```

`main.wxs` references the harvested `GstRuntime` component group, so step 1 must
run before step 2 (CI does both — see `.github/workflows/release.yml`).

The installer is written to `target/wix/kuvatin-<version>-x86_64.msi`
(e.g. `kuvatin-1.5.0-x86_64.msi`), now ~106 MB because it carries the GStreamer
runtime. It is under `target/`, which is gitignored and not committed.

## Regenerating main.wxs

`main.wxs` was generated with `cargo wix init --force -p kuvatin` and then
hand-edited to add the `KuvatinRegister` / `KuvatinUnregister` deferred custom
actions and their `InstallExecuteSequence`. If you regenerate it, re-apply those
customizations. The custom actions reference the main executable by its `File`
`Id` (`exe0`) via `FileKey`.
