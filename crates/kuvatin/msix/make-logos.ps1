# Render the sparse package's logos from the app icon (run once when the icon
# changes; the PNGs are committed). ASCII-only on purpose: PowerShell 5.1
# reads BOM-less scripts as ANSI, and the repo path is not ASCII.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$src = [System.Drawing.Image]::FromFile((Join-Path $PSScriptRoot '..\assets\kuvatin-icon.png'))
$outDir = Join-Path $PSScriptRoot 'Assets'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
foreach ($spec in @(@{ n = 'Square44x44Logo.png'; s = 44 }, @{ n = 'Square150x150Logo.png'; s = 150 }, @{ n = 'StoreLogo.png'; s = 50 })) {
    $bmp = New-Object System.Drawing.Bitmap $spec.s, $spec.s
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.Clear([System.Drawing.Color]::Transparent)
    $g.DrawImage($src, 0, 0, $spec.s, $spec.s)
    $bmp.Save((Join-Path $outDir $spec.n), [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
}
$src.Dispose()
Get-ChildItem $outDir | Select-Object Name, Length | Format-Table -HideTableHeaders
