<#
.SYNOPSIS
  Cut a release: bump the version everywhere it lives, commit, tag.

.DESCRIPTION
  The version is stamped in three places that CI cross-checks on a tag run
  (Cargo.toml, the landing page's JSON-LD, and the tag itself). This keeps
  them in step in one go:

    scripts\release.ps1 2.8.0          # bump + commit + tag, print the push
    scripts\release.ps1 2.8.0 -Push    # ...and push master + the tag

  Pushing the tag starts the release pipeline (tests, MSI, install test,
  publish). Release notes: `gh release edit v2.8.0 --notes-file notes.md`
  once the run has created the release.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string] $Version,
    [switch] $Push
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if ((git rev-parse --abbrev-ref HEAD) -ne 'master') { throw 'release from master' }
if (git status --porcelain) { throw 'working tree is not clean' }
if (git tag -l "v$Version") { throw "tag v$Version already exists" }

$cargo = 'Cargo.toml'
$page = 'docs/index.html'
$c = [IO.File]::ReadAllText((Resolve-Path $cargo))
$p = [IO.File]::ReadAllText((Resolve-Path $page))
$cNew = [regex]::Replace($c, '(?m)^version = "\d+\.\d+\.\d+"$', "version = `"$Version`"", 1)
$pNew = [regex]::Replace($p, '"softwareVersion": "\d+\.\d+\.\d+"', "`"softwareVersion`": `"$Version`"", 1)
if ($cNew -eq $c) { throw "no workspace version line found in $cargo" }
if ($pNew -eq $p) { throw "no softwareVersion found in $page" }
$utf8 = New-Object Text.UTF8Encoding $false
[IO.File]::WriteAllText((Resolve-Path $cargo), $cNew, $utf8)
[IO.File]::WriteAllText((Resolve-Path $page), $pNew, $utf8)

# Cargo.lock carries the workspace version too. Run cargo through cmd: under
# Windows PowerShell 5.1 a native command writing to stderr (cargo's
# "Locking N packages" line) becomes a terminating error when redirected.
cmd /c "cargo update --workspace --offline >nul 2>nul"
if ($LASTEXITCODE -ne 0) { cmd /c "cargo update --workspace >nul 2>nul" }
if ($LASTEXITCODE -ne 0) { throw "cargo update failed" }

git add Cargo.toml Cargo.lock docs/index.html
git commit -q -m "release: bump workspace version to $Version"
git tag -a "v$Version" -m "Kuvatin $Version"
Write-Host "committed and tagged v$Version"

if ($Push) {
    git push origin master
    git push origin "v$Version"
    Write-Host "pushed; the release run is starting: gh run watch"
} else {
    Write-Host "to publish:  git push origin master; git push origin v$Version"
}
