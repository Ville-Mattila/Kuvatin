<#
.SYNOPSIS
  Add the components of one CycloneDX JSON document to another, in place.

  The release SBOM is a scan of the source tree, which knows the Rust crates
  but not the GStreamer runtime the installer bundles. bundle-gstreamer.ps1
  describes exactly which runtime files it staged; this appends that
  description, so the one SBOM a release ships names what the installer
  actually puts on disk.

  Runs under PowerShell 7 in CI. Windows PowerShell 5.1 works for small files,
  but its JSON reader stops at about 2 MB.
#>
param(
    [Parameter(Mandatory = $true)][string]$Sbom,
    [Parameter(Mandatory = $true)][string]$Add
)
$ErrorActionPreference = "Stop"
# Windows PowerShell 5.1 can write an array as {"value":[...],"Count":n}
# because of an extended Count property. PowerShell 7 never does.
if ($PSVersionTable.PSVersion.Major -lt 6) { Remove-TypeData System.Array -ErrorAction SilentlyContinue }

# .NET resolves relative paths against the process directory, not PowerShell's.
$Sbom = (Resolve-Path $Sbom).Path
$Add = (Resolve-Path $Add).Path

$main = Get-Content -Raw -Path $Sbom | ConvertFrom-Json
$extra = Get-Content -Raw -Path $Add | ConvertFrom-Json
foreach ($doc in @(@{ Path = $Sbom; Json = $main }, @{ Path = $Add; Json = $extra })) {
    if ($doc.Json.bomFormat -ne 'CycloneDX') { throw "$($doc.Path) is not a CycloneDX document" }
}

$existing = @{}
foreach ($c in @($main.components)) {
    if ($c -and $c.'bom-ref') { $existing[$c.'bom-ref'] = $true }
}
$adding = @(@($extra.components) | Where-Object { $_ })
if (-not $adding.Count) { throw "$Add has no components to add" }
foreach ($c in $adding) {
    if ($existing.ContainsKey($c.'bom-ref')) { throw "$Sbom already has a component $($c.'bom-ref')" }
}

$merged = @(@($main.components) | Where-Object { $_ }) + $adding
if ($main.PSObject.Properties.Name -contains 'components') {
    $main.components = $merged
} else {
    $main | Add-Member -NotePropertyName components -NotePropertyValue $merged
}

[IO.File]::WriteAllText($Sbom, ($main | ConvertTo-Json -Depth 64), (New-Object Text.UTF8Encoding $false))
Write-Host ("{0}: {1} top-level components after adding {2} from {3}" -f $Sbom, $merged.Count, $adding.Count, $Add)
