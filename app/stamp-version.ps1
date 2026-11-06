<#
  Stamps a release version into the Tauri project (used by CI and by build.cmd).

    pwsh app/stamp-version.ps1 -Version 0.2.0 [-Root <dir containing src-tauri>]

  Rewrites src-tauri/tauri.conf.json ("version"), src-tauri/Cargo.toml ([package]
  version) and the `blinker` entry of src-tauri/Cargo.lock so `--locked` builds
  keep working. Fails loudly when a pattern is not found.
#>
param(
    [Parameter(Mandatory)][string]$Version,
    [string]$Root = $PSScriptRoot
)
$ErrorActionPreference = 'Stop'
if ($Version -notmatch '^\d+\.\d+\.\d+([-+].+)?$') { throw "Bad version '$Version'" }

function Stamp([string]$Path, [string]$Pattern, [string]$Replacement) {
    $text = [IO.File]::ReadAllText($Path)
    if (-not [regex]::IsMatch($text, $Pattern)) { throw "Failed to stamp version in $Path (pattern not found)" }
    $new = ([regex]$Pattern).Replace($text, $Replacement, 1)   # first match only
    [IO.File]::WriteAllText($Path, $new, (New-Object Text.UTF8Encoding($false)))
}

$tauri = Join-Path $Root 'src-tauri'
Stamp (Join-Path $tauri 'tauri.conf.json') '(?m)^(\s*"version"\s*:\s*")[^"]*(")' "`${1}$Version`${2}"
Stamp (Join-Path $tauri 'Cargo.toml') '(?m)^(version\s*=\s*")[^"]*(")' "`${1}$Version`${2}"
Stamp (Join-Path $tauri 'Cargo.lock') '(?m)^(name = "blinker"\r?\nversion = ")[^"]*(")' "`${1}$Version`${2}"
Write-Host "Stamped version $Version"
