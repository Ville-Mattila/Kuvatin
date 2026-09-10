<#
.SYNOPSIS
  Build Kuvatin.msix, the sparse package that puts the Windows 11 context-menu
  handler on the top-level menu.

.DESCRIPTION
  Fills the AppxManifest.xml template (version, publisher, handler CLSID, one
  ItemType per extension the Explorer menu attaches to, plus Directory and
  Directory\Background), copies the logos, and packs with MakeAppx.exe from the
  Windows SDK.

  The package MUST be signed before Windows will register it: it declares a COM
  server ("executable activations"), which Windows 11 refuses in an unsigned
  package (0x80073D2B). -Publisher must equal the signing certificate's Subject
  exactly. Sign the output with:

    signtool sign /fd SHA256 /f cert.pfx /p <password> Kuvatin.msix

  and make the certificate trusted on the target machine (a CA-issued
  code-signing certificate, or a self-signed one imported into the
  Trusted People store). The .msix is installed next to kuvatin.exe and
  registered per user by `kuvatin.exe --register`.

    crates\kuvatin\msix\build-msix.ps1 -Version 2.9.0 -Out target\msix\Kuvatin.msix

  The extension list below is checked against the engine by a unit test in
  crates\kuvatin\src\shell\windows.rs (menu_extensions), so it cannot drift.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [ValidatePattern('^\d+\.\d+\.\d+$')] [string] $Version,
    [Parameter(Mandatory = $true)] [string] $Out,
    [string] $Publisher = 'CN=Ville Mattila',
    [string] $Clsid = '7A3E2B6C-9D14-4F58-8B2A-1C6E5D4F3A90'
)
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot

$makeappx = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\makeappx.exe" -ErrorAction SilentlyContinue |
    Sort-Object { [version]($_.Directory.Parent.Name) } | Select-Object -Last 1
if (-not $makeappx) { throw 'MakeAppx.exe not found: install the Windows 10/11 SDK' }

# EXTENSIONS: image inputs, then the sequence-only frame formats (see the test).
$extensions = '.png', '.jpg', '.jpeg', '.jpe', '.jfif', '.webp', '.bmp', '.tiff', '.tif', '.gif', '.exr'
$types = @($extensions) + @('Directory', 'Directory\Background')
$items = ($types | ForEach-Object {
    "            <desktop5:ItemType Type=`"$_`">`n              <desktop5:Verb Id=`"Kuvatin`" Clsid=`"$Clsid`" />`n            </desktop5:ItemType>"
}) -join "`n"

$stage = Join-Path ([IO.Path]::GetTempPath()) "kuvatin-msix-$PID"
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Path (Join-Path $stage 'Assets') -Force | Out-Null
Copy-Item (Join-Path $here 'Assets\*.png') (Join-Path $stage 'Assets')

$manifest = [IO.File]::ReadAllText((Join-Path $here 'AppxManifest.xml'))
$manifest = $manifest.Replace('@VERSION@', "$Version.0").Replace('@PUBLISHER@', $Publisher).Replace('@CLSID@', $Clsid).Replace('@ITEM_TYPES@', $items)
[IO.File]::WriteAllText((Join-Path $stage 'AppxManifest.xml'), $manifest, (New-Object Text.UTF8Encoding $false))

New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Out) | Out-Null
$Out = Join-Path (Resolve-Path (Split-Path -Parent $Out)).Path (Split-Path -Leaf $Out)
if (Test-Path $Out) { Remove-Item $Out -Force }
# /nv: the manifest references files that live in the external location, not in the package.
& $makeappx.FullName pack /o /d $stage /nv /p $Out
if ($LASTEXITCODE -ne 0) { throw "makeappx exited $LASTEXITCODE" }
Remove-Item $stage -Recurse -Force
Write-Host "built $Out ($((Get-Item $Out).Length) bytes, $($types.Count) item types, publisher $Publisher; unsigned until signtool runs)"
