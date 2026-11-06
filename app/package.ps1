<#
  Packages the release exe as the updater expects it.

    pwsh app/package.ps1 -Tag v0.2.0 -OutDir dist [-Exe <path to BlinkerUI.exe>]

  Produces in OutDir:
    BlinkerUI-<Tag>.zip         ONE top-level folder BlinkerUI/ holding BlinkerUI.exe,
                                icons/, logo.png, logo.ico (old PowerShell-updater
                                installs take the FIRST directory of the zip)
    BlinkerUI-<Tag>.zip.sha256  lowercase hex SHA-256, ASCII, no newline
  Entry names use forward slashes (zip spec).
#>
param(
    [Parameter(Mandatory)][string]$Tag,
    [Parameter(Mandatory)][string]$OutDir,
    [string]$Exe
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
if (-not $Exe) { $Exe = Join-Path $PSScriptRoot 'src-tauri\target\release\BlinkerUI.exe' }
foreach ($p in @($Exe, (Join-Path $repo 'icons'), (Join-Path $repo 'logo.png'), (Join-Path $repo 'logo.ico'))) {
    if (-not (Test-Path $p)) { throw "Missing input: $p" }
}

New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
$zip = Join-Path $OutDir "BlinkerUI-$Tag.zip"
Remove-Item $zip, "$zip.sha256" -Force -ErrorAction SilentlyContinue

$stage = Join-Path $OutDir 'stage'
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
$app = Join-Path $stage 'BlinkerUI'
New-Item -ItemType Directory -Force $app | Out-Null
Copy-Item $Exe (Join-Path $app 'BlinkerUI.exe')
Copy-Item (Join-Path $repo 'icons') (Join-Path $app 'icons') -Recurse
Copy-Item (Join-Path $repo 'logo.png'), (Join-Path $repo 'logo.ico') $app

Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$z = [IO.Compression.ZipFile]::Open($zip, 'Create')
try {
    Get-ChildItem $app -Recurse -File | Sort-Object FullName | ForEach-Object {
        $rel = $_.FullName.Substring($stage.Length).TrimStart('\', '/') -replace '\\', '/'
        [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile($z, $_.FullName, $rel, 'Optimal')
    }
} finally { $z.Dispose() }
Remove-Item $stage -Recurse -Force

$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
[IO.File]::WriteAllText("$zip.sha256", $hash, [Text.Encoding]::ASCII)
Write-Host "Wrote $zip"
Write-Host "sha256 $hash"
