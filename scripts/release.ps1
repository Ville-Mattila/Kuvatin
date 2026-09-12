<#
.SYNOPSIS
  Cut a release: bump the version everywhere it lives, commit, tag.

.DESCRIPTION
  The version is stamped in three places that CI cross-checks on a tag run
  (Cargo.toml, the landing page's JSON-LD, and the tag itself). This keeps
  them in step in one go:

    scripts\release.ps1 2.8.0          # bump + commit + tag, print the push
    scripts\release.ps1 2.8.0 -Push    # ...and push master + the tag
    scripts\release.ps1 2.8.0 -SkipChecks   # re-run after fixing a failed gate

  The same format, lint and test gates CI runs happen FIRST, before anything
  is committed or tagged: a tag CI is going to reject is better caught here,
  where nothing has been written yet. (The video suite needs GStreamer on
  PATH and is left to CI.)

  Pushing the tag starts the release pipeline (tests, MSI, install test,
  publish). Release notes: `gh release edit v2.8.0 --notes-file notes.md`
  once the run has created the release.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string] $Version,
    [switch] $Push,
    # Skip the local gates. For re-running after a gate failed and was fixed
    # by hand — not for skipping the gates.
    [switch] $SkipChecks
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if ((git rev-parse --abbrev-ref HEAD) -ne 'master') { throw 'release from master' }
if (git status --porcelain) { throw 'working tree is not clean' }
if (git tag -l "v$Version") { throw "tag v$Version already exists" }

# The gates, before the tree is touched. A failure here costs a few minutes;
# a failure after the tag is pushed costs a version number.
if (-not $SkipChecks) {
    $gates = @(
        @{ What = 'format'; Args = @('fmt', '--all', '--check') },
        @{ What = 'clippy'; Args = @('clippy', '--workspace', '--all-targets', '--', '-D', 'warnings') },
        @{ What = 'tests';  Args = @('test', '-p', 'kuvatin-core', '-p', 'kuvatin', '--release') }
    )
    foreach ($gate in $gates) {
        Write-Host "gate: $($gate.What)"
        # Cargo reports progress ("Checking kuvatin-core...") on stderr, and
        # under Windows PowerShell a native command that writes to stderr while
        # $ErrorActionPreference is 'Stop' becomes a terminating error - so the
        # gate "failed" with clippy perfectly happy. The exit code is the only
        # thing worth reading here.
        $ErrorActionPreference = 'Continue'
        & cargo $gate.Args
        $code = $LASTEXITCODE
        $ErrorActionPreference = 'Stop'
        if ($code -ne 0) { throw "$($gate.What) failed - not tagging" }
    }
}

# CHANGELOG.md is what the pipeline publishes as the release body. The
# "Unreleased" section becomes this version's section, dated today; if there is
# nothing under it, there is nothing to release.
$log = 'CHANGELOG.md'
$l = [IO.File]::ReadAllText((Resolve-Path $log))
$today = (Get-Date).ToString('yyyy-MM-dd')
$un = [regex]::Match($l, '(?ms)^## \[Unreleased\]\s*\n(.*?)(?=^## \[|\z)')
if (-not $un.Success) { throw "no Unreleased section in $log" }
if (-not $un.Groups[1].Value.Trim()) { throw "the Unreleased section in $log is empty - write the notes first" }
$lNew = [regex]::Replace($l, '(?m)^## \[Unreleased\]\s*$', "## [Unreleased]`r`n`r`n## [$Version] - $today", 1)
# ...and the link definitions at the bottom follow.
$lNew = [regex]::Replace($lNew, '(?m)^\[Unreleased\]: (.*)compare/v[\d.]+\.\.\.HEAD\s*$',
    "[Unreleased]: `$1compare/v$Version...HEAD`r`n[$Version]: `$1releases/tag/v$Version", 1)
if ($lNew -eq $l) { throw "could not move the Unreleased section in $log" }

$cargo = 'Cargo.toml'
$page = 'docs/index.html'
$c = [IO.File]::ReadAllText((Resolve-Path $cargo))
$p = [IO.File]::ReadAllText((Resolve-Path $page))
# (?=\r?$): git's autocrlf can leave Cargo.toml with CRLF endings.
$cNew = [regex]::Replace($c, '(?m)^version = "\d+\.\d+\.\d+"(?=\r?$)', "version = `"$Version`"", 1)
$pNew = [regex]::Replace($p, '"softwareVersion": "\d+\.\d+\.\d+"', "`"softwareVersion`": `"$Version`"", 1)
if ($cNew -eq $c) { throw "no workspace version line found in $cargo" }
if ($pNew -eq $p) { throw "no softwareVersion found in $page" }
$utf8 = New-Object Text.UTF8Encoding $false
[IO.File]::WriteAllText((Resolve-Path $cargo), $cNew, $utf8)
[IO.File]::WriteAllText((Resolve-Path $page), $pNew, $utf8)
[IO.File]::WriteAllText((Resolve-Path $log), $lNew, $utf8)

# Cargo.lock carries the workspace version too. Run cargo through cmd: under
# Windows PowerShell 5.1 a native command writing to stderr (cargo's
# "Locking N packages" line) becomes a terminating error when redirected.
cmd /c "cargo update --workspace --offline >nul 2>nul"
if ($LASTEXITCODE -ne 0) { cmd /c "cargo update --workspace >nul 2>nul" }
if ($LASTEXITCODE -ne 0) { throw "cargo update failed" }

git add Cargo.toml Cargo.lock docs/index.html CHANGELOG.md
git commit -q -m "release: bump workspace version to $Version"

# Order matters when pushing: master first, and the tag only once that push
# has been accepted. A rejected push (someone else got there first) then
# leaves an ordinary local commit to rebase, rather than a tag pointing at a
# commit the remote has never seen.
if ($Push) {
    git push origin master
    if ($LASTEXITCODE -ne 0) {
        throw "push rejected - the bump is committed locally; rebase, then re-run with -SkipChecks"
    }
}
git tag -a "v$Version" -m "Kuvatin $Version"
Write-Host "committed and tagged v$Version"

if ($Push) {
    git push origin "v$Version"
    if ($LASTEXITCODE -ne 0) { throw "the tag did not push; retry: git push origin v$Version" }
    Write-Host "pushed; the release run is starting: gh run watch"
} else {
    Write-Host "to publish:  git push origin master; git push origin v$Version"
}
