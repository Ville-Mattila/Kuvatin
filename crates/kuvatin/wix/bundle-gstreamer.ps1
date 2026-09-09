<#
.SYNOPSIS
  Stage a TRIMMED GStreamer runtime (an allow-list of plugins plus the DLLs
  they and kuvatin.exe actually import) together with every component's
  license text, and harvest it into a WiX fragment so the installer bundles it
  next to kuvatin.exe.

  The core DLLs must sit next to the exe (they are load-time dependencies,
  loaded before main runs). Plugins go in a `gstreamer-plugins` subdir, which
  the app points GST_PLUGIN_PATH at (see main.rs::configure_bundled_gstreamer).
  License texts go in a `licenses` subdir (one folder per component, as
  upstream ships them) next to a generated THIRD-PARTY-NOTICES.txt.

  Why an allow-list: the upstream distribution is 391 DLLs / ~270 MB and
  includes encoders Kuvatin never uses (x265, a52dec, openh264, SvtAv1...),
  network stacks, and bindings (gstpython). Bundling only what is loaded keeps
  the installer small and the license/patent surface honest. The list was
  derived empirically (GST_PLUGIN_LOADING logs across the video test suite and
  decode/discovery of every supported container/codec) plus the elements GES
  and playbin request lazily. To bundle everything anyway: -AllPlugins.

  Flat-DLL closure: rather than guess which of the ~130 bin\ DLLs a plugin
  needs, the script walks PE import tables (regular + delay-load) starting from
  kuvatin.exe and every allow-listed plugin, transitively, keeping only DLLs
  that exist in bin\ (system DLLs are left to Windows).

  Output:
    <StageDir>\*.dll                      (core + dependency DLLs, flat)
    <StageDir>\gstreamer-plugins\*.dll    (plugins)
    <StageDir>\licenses\<component>\*     (license texts, all components)
    <StageDir>\THIRD-PARTY-NOTICES.txt    (what is bundled + source offer)
    <OutWxs>                              (heat-generated fragment, ComponentGroup "GstRuntime")

  Run before `cargo wix`, then build with:
    cargo wix -p kuvatin --compiler-arg "-dGstStageDir=<StageDir>"
#>
param(
    [string]$GstRoot  = "C:\Program Files\gstreamer\1.0\msvc_x86_64",
    [string]$StageDir = (Join-Path (Resolve-Path "$PSScriptRoot\..\..\..").Path "target\gst-staging"),
    [string]$OutWxs   = (Join-Path $PSScriptRoot "gstreamer.wxs"),
    [string]$HeatExe  = "heat",
    # The built app; its imports seed the DLL closure.
    [string]$AppExe   = (Join-Path (Resolve-Path "$PSScriptRoot\..\..\..").Path "target\release\kuvatin.exe"),
    # Escape hatch: stage every plugin and every bin DLL (the pre-2.7 behaviour).
    [switch]$AllPlugins
)
$ErrorActionPreference = "Stop"

if (-not (Test-Path "$GstRoot\bin")) { throw "GStreamer not found at $GstRoot" }
if (-not $AllPlugins -and -not (Test-Path $AppExe)) { throw "app exe not found at $AppExe (build release first, or pass -AppExe)" }

# ---------------------------------------------------------------------------
# Plugin allow-list (file names without .dll). Grouped by what needs them.
# ---------------------------------------------------------------------------
$Plugins = @(
    # core / playback / app glue
    'gstcoreelements', 'gstplayback', 'gstapp', 'gsttypefindfunctions', 'gstpbtypes',
    'gstautodetect', 'gstautoconvert', 'gstencoding',
    # loaded in practice by playbin/decodebin/GES on Windows (keeps behaviour identical to the full bundle)
    'gstd3d11', 'gstd3d12', 'gstopengl', 'gstgdkpixbuf', 'gstaudiofx', 'gstsoundtouch', 'gstvideomixer', 'gstcodectimestamper',
    # GES editing (timeline, compositing, gap filling)
    'gstges', 'gstnle', 'gstcompositor', 'gstaudiomixer', 'gstvideotestsrc', 'gstaudiotestsrc',
    'gstimagefreeze', 'gstvideoconvertscale', 'gstvideorate', 'gstaudioconvert',
    'gstaudioresample', 'gstaudiorate', 'gstvolume', 'gstvideofilter', 'gstvideocrop',
    'gstvideobox', 'gstalpha', 'gstalphacolor', 'gstdeinterlace', 'gstinterleave',
    # audio output on Windows
    'gstwasapi2', 'gstwasapi', 'gstdirectsound',
    # containers
    'gstisomp4', 'gstmatroska', 'gstavi', 'gstasf', 'gstogg', 'gstwavparse', 'gstflv',
    # parsers
    'gstvideoparsersbad', 'gstaudioparsers', 'gstopusparse', 'gstjpegformat',
    # decoders (libav covers h264/hevc/aac/mp3/wmv/prores/bmp/tiff/webp/gif...)
    'gstlibav', 'gstvpx', 'gstopus', 'gstvorbis', 'gstflac', 'gstmpg123', 'gstpng', 'gstjpeg',
    'gstdav1d', 'gstsubparse',
    # still images / sequences
    'gstmultifile',
    # encoders the export UI offers (H.264 hardware+software, VP8/VP9, AAC, Opus)
    'gstnvcodec', 'gstx264', 'gstvoaacenc',
    # tags (id3 in mp3, metadata in mkv/mp4)
    'gstid3demux', 'gstid3tag', 'gsttaglib', 'gstapetag', 'gsticydemux'
)

Write-Host "Staging GStreamer runtime from $GstRoot -> $StageDir"
if (Test-Path $StageDir) { Remove-Item $StageDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $StageDir | Out-Null
$pluginDir = Join-Path $StageDir "gstreamer-plugins"
New-Item -ItemType Directory -Force -Path $pluginDir | Out-Null

# ---------------------------------------------------------------------------
# PE import walker (regular + delay-load imports). Pure PowerShell so CI needs
# no dumpbin. Returns the imported DLL names of one PE file.
# ---------------------------------------------------------------------------
function Get-PeImports([string]$Path) {
    $b = [IO.File]::ReadAllBytes($Path)
    $pe = [BitConverter]::ToInt32($b, 0x3C)
    if ($pe -le 0 -or $pe + 4 -gt $b.Length -or [BitConverter]::ToUInt32($b, $pe) -ne 0x4550) { return @() }
    $coff = $pe + 4
    $nsec = [BitConverter]::ToUInt16($b, $coff + 2)
    $optSize = [BitConverter]::ToUInt16($b, $coff + 16)
    $opt = $coff + 20
    $magic = [BitConverter]::ToUInt16($b, $opt)
    $dd = if ($magic -eq 0x20B) { $opt + 112 } else { $opt + 96 }   # PE32+ vs PE32 data directories
    $sections = @()
    $so = $opt + $optSize
    for ($i = 0; $i -lt $nsec; $i++) {
        $s = $so + $i * 40
        $sections += [pscustomobject]@{
            VA = [BitConverter]::ToUInt32($b, $s + 12); VSize = [BitConverter]::ToUInt32($b, $s + 8)
            Raw = [BitConverter]::ToUInt32($b, $s + 20); RawSize = [BitConverter]::ToUInt32($b, $s + 16)
        }
    }
    function RvaToOff([uint32]$rva) {
        foreach ($s in $sections) {
            $span = [Math]::Max($s.VSize, $s.RawSize)
            if ($rva -ge $s.VA -and $rva -lt ($s.VA + $span)) { return [int]($s.Raw + ($rva - $s.VA)) }
        }
        return -1
    }
    function ReadCStr([int]$off) {
        if ($off -lt 0 -or $off -ge $b.Length) { return $null }
        $e = $off; while ($e -lt $b.Length -and $b[$e] -ne 0) { $e++ }
        return [Text.Encoding]::ASCII.GetString($b, $off, $e - $off)
    }
    $names = @()
    # data directory [1] = import table: IMAGE_IMPORT_DESCRIPTOR (20 B), Name RVA at +12
    $impRva = [BitConverter]::ToUInt32($b, $dd + 1 * 8)
    if ($impRva -ne 0) {
        $d = RvaToOff $impRva
        while ($d -ge 0 -and $d + 20 -le $b.Length) {
            $nameRva = [BitConverter]::ToUInt32($b, $d + 12)
            if ($nameRva -eq 0) { break }
            $n = ReadCStr (RvaToOff $nameRva); if ($n) { $names += $n }
            $d += 20
        }
    }
    # data directory [13] = delay-load imports: ImgDelayDescr (32 B), DllName RVA at +4
    $dlyRva = [BitConverter]::ToUInt32($b, $dd + 13 * 8)
    if ($dlyRva -ne 0) {
        $d = RvaToOff $dlyRva
        while ($d -ge 0 -and $d + 32 -le $b.Length) {
            $nameRva = [BitConverter]::ToUInt32($b, $d + 4)
            if ($nameRva -eq 0) { break }
            $n = ReadCStr (RvaToOff $nameRva); if ($n) { $names += $n }
            $d += 32
        }
    }
    return $names
}

# ---------------------------------------------------------------------------
# Plugins
# ---------------------------------------------------------------------------
$pluginFiles = @()
if ($AllPlugins) {
    $pluginFiles = Get-ChildItem "$GstRoot\lib\gstreamer-1.0\*.dll"
} else {
    $missing = @()
    foreach ($p in $Plugins) {
        $f = Join-Path "$GstRoot\lib\gstreamer-1.0" "$p.dll"
        if (Test-Path $f) { $pluginFiles += Get-Item $f } else { $missing += $p }
    }
    if ($missing.Count) { throw "allow-listed plugins missing from $GstRoot : $($missing -join ', ')" }
}
$pluginFiles | Copy-Item -Destination $pluginDir -Force
Write-Host ("Plugins: {0} staged" -f $pluginFiles.Count)

# ---------------------------------------------------------------------------
# Flat DLLs: everything in bin\ when -AllPlugins, else the import closure of
# the app + staged plugins, restricted to DLLs that exist in bin\.
# ---------------------------------------------------------------------------
$binDlls = @{}
Get-ChildItem "$GstRoot\bin\*.dll" | ForEach-Object { $binDlls[$_.Name.ToLower()] = $_.FullName }
if ($AllPlugins) {
    $binDlls.Values | Copy-Item -Destination $StageDir -Force
} else {
    $needed = @{}
    $queue = New-Object System.Collections.Generic.Queue[string]
    $seeds = @($AppExe) + ($pluginFiles | ForEach-Object { $_.FullName })
    foreach ($seed in $seeds) { foreach ($imp in Get-PeImports $seed) { $queue.Enqueue($imp.ToLower()) } }
    while ($queue.Count) {
        $n = $queue.Dequeue()
        if ($needed.ContainsKey($n) -or -not $binDlls.ContainsKey($n)) { continue }
        $needed[$n] = $binDlls[$n]
        foreach ($imp in Get-PeImports $binDlls[$n]) { $queue.Enqueue($imp.ToLower()) }
    }
    $needed.Values | Sort-Object | Copy-Item -Destination $StageDir -Force
    Write-Host ("Flat DLLs: {0} of {1} in bin\ reachable from the app and plugins" -f $needed.Count, $binDlls.Count)
}

# ---------------------------------------------------------------------------
# Licenses: every component's texts, as upstream ships them, plus a notices
# file naming what is bundled and where the corresponding source lives.
# ---------------------------------------------------------------------------
$licSrc = Join-Path $GstRoot "share\licenses"
if (-not (Test-Path $licSrc)) { throw "license directory not found at $licSrc" }
Copy-Item $licSrc -Destination (Join-Path $StageDir "licenses") -Recurse -Force
$components = Get-ChildItem (Join-Path $StageDir "licenses") -Directory | Select-Object -ExpandProperty Name
# The DLLs carry no ProductVersion; pkg-config metadata is authoritative.
$gstVersion = (Select-String -Path (Join-Path $GstRoot "lib\pkgconfig\gstreamer-1.0.pc") -Pattern '^Version:\s*(\S+)' | Select-Object -First 1).Matches.Groups[1].Value
if (-not $gstVersion) { throw "could not read the GStreamer version from lib\pkgconfig\gstreamer-1.0.pc" }
$stagedPlugins = Get-ChildItem $pluginDir -Filter *.dll | Select-Object -ExpandProperty Name | Sort-Object
$stagedFlat = Get-ChildItem $StageDir -Filter *.dll | Select-Object -ExpandProperty Name | Sort-Object
$notices = @"
THIRD-PARTY NOTICES FOR KUVATIN
================================

Kuvatin itself is free software under the GNU General Public License,
version 3 or later (see License.rtf / https://www.gnu.org/licenses/gpl-3.0.html).

This installation also contains a subset of the GStreamer $gstVersion runtime for
Windows (MSVC x86_64) - unmodified binaries from the official distribution at
https://gstreamer.freedesktop.org/download/ - namely the shared libraries
listed below (next to kuvatin.exe) and the plugins in gstreamer-plugins\.
These components are licensed under their own terms: mostly the GNU Lesser
General Public License 2.1 or later (GStreamer, the FFmpeg build, GLib, ...),
some plugins and libraries under the GNU GPL 2.0 or later (for example x264),
and others under BSD, MIT, MPL, Apache or similar permissive licenses.

The complete license text and copyright notices for EVERY component of the
GStreamer distribution are installed in the licenses\ folder next to this
file, one folder per component ($($components.Count) components).

Corresponding source
--------------------
The GStreamer modules (gstreamer, gst-plugins-base/-good/-bad/-ugly,
gst-libav, gst-editing-services, ...) version $gstVersion are available from
  https://gstreamer.freedesktop.org/src/
The Windows binaries are produced by GStreamer's Cerbero build system, whose
recipes identify the exact upstream source of every third-party library:
  https://gitlab.freedesktop.org/gstreamer/cerbero  (tag $gstVersion)
Kuvatin's own source is at https://github.com/Ville-Mattila/Kuvatin. Should any
of the above be unavailable, corresponding source for the bundled components
will be provided on request via that repository's issue tracker.

Rust dependencies compiled into kuvatin.exe are listed with their licenses in
the source tree (Cargo.lock; each crate's license file in its published
package). libimagequant is GPL-3.0-or-later; the remainder are MIT/Apache-2.0,
BSD or similarly permissive.

Bundled plugins ($($stagedPlugins.Count))
----------------------------------------
$($stagedPlugins -join "`r`n")

Bundled shared libraries ($($stagedFlat.Count))
----------------------------------------
$($stagedFlat -join "`r`n")
"@
Set-Content -Path (Join-Path $StageDir "THIRD-PARTY-NOTICES.txt") -Value $notices -Encoding utf8

$dllCount = (Get-ChildItem $StageDir -Recurse -Filter *.dll).Count
$mb = [math]::Round(((Get-ChildItem $StageDir -Recurse -File | Measure-Object Length -Sum).Sum / 1MB), 1)
Write-Host "Staged $dllCount DLLs + $($components.Count) license folders ($mb MB)"

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
