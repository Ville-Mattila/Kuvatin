<#
.SYNOPSIS
  Stage the GStreamer runtime DLLs + plugins and harvest them into a WiX
  fragment so the installer bundles them next to kuvatin.exe.

  The core DLLs must sit next to the exe (they are load-time dependencies,
  loaded before main runs). Plugins go in a `gstreamer-plugins` subdir, which
  the app points GST_PLUGIN_PATH at (see main.rs::configure_bundled_gstreamer).

  Output:
    <StageDir>\*.dll                      (core + dependency DLLs, flat)
    <StageDir>\gstreamer-plugins\*.dll    (plugins)
    <OutWxs>                              (heat-generated fragment, ComponentGroup "GstRuntime")

  Run before `cargo wix`, then build with:
    cargo wix -p kuvatin --include wix\gstreamer.wxs --compiler-arg "-dGstStageDir=<StageDir>"
#>
param(
    [string]$GstRoot  = "C:\Program Files\gstreamer\1.0\msvc_x86_64",
    [string]$StageDir = (Join-Path (Resolve-Path "$PSScriptRoot\..\..\..").Path "target\gst-staging"),
    [string]$OutWxs   = (Join-Path $PSScriptRoot "gstreamer.wxs"),
    [string]$HeatExe  = "heat"
)
$ErrorActionPreference = "Stop"

if (-not (Test-Path "$GstRoot\bin")) { throw "GStreamer not found at $GstRoot" }

Write-Host "Staging GStreamer runtime from $GstRoot -> $StageDir"
if (Test-Path $StageDir) { Remove-Item $StageDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $StageDir | Out-Null
$plugins = Join-Path $StageDir "gstreamer-plugins"
New-Item -ItemType Directory -Force -Path $plugins | Out-Null

# Core + dependency DLLs (flat, next to the exe).
Copy-Item "$GstRoot\bin\*.dll" -Destination $StageDir -Force
# Plugins (loaded at gst::init via GST_PLUGIN_PATH).
Copy-Item "$GstRoot\lib\gstreamer-1.0\*.dll" -Destination $plugins -Force

$dllCount = (Get-ChildItem $StageDir -Recurse -Filter *.dll).Count
$mb = [math]::Round(((Get-ChildItem $StageDir -Recurse -Filter *.dll | Measure-Object Length -Sum).Sum / 1MB), 1)
Write-Host "Staged $dllCount DLLs ($mb MB)"

# Harvest into a ComponentGroup rooted at the exe's Bin directory. -srd drops the
# staging root so contents land directly in Bin; -var lets candle resolve the
# source path at build time.
# NOTE: -gg generates FRESH component GUIDs on every build. That is ONLY safe
# because main.wxs uses MajorUpgrade Schedule='afterInstallInitialize' (full
# uninstall of the old product before the new one installs). If that schedule
# ever changes, switch to stable GUIDs here or upgrades will orphan files.
Write-Host "Harvesting -> $OutWxs"
& $HeatExe dir $StageDir `
    -nologo -gg -srd -sreg -scom `
    -dr Bin -cg GstRuntime `
    -var var.GstStageDir `
    -out $OutWxs
if ($LASTEXITCODE -ne 0) { throw "heat failed ($LASTEXITCODE)" }
Write-Host "Done. StageDir=$StageDir"
