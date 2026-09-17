# Vortex release helper — bumps version, rebuilds UI + Rust, bundles NSIS installer.
# Usage:  powershell -ExecutionPolicy Bypass -File scripts\release.ps1 -Version 1.0.1 [-Tag]
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [switch]$Tag
)

$ErrorActionPreference = "Stop"
$Repo  = Split-Path -Parent $PSScriptRoot
$Utf8  = New-Object System.Text.UTF8Encoding($false)   # no BOM
$now   = Get-Date -Format "yyyy-MM-dd HH:mm:ss"

if ($Version -notmatch '^\d+\.\d+\.\d+$') {
    throw "Version must look like 1.0.1 (got '$Version')"
}

Write-Host "[$now] Bumping version to $Version" -ForegroundColor Cyan

function Bump-Json([string]$Path) {
    $c = [System.IO.File]::ReadAllText($Path)
    $c = [regex]::Replace($c, '"version"\s*:\s*"[^"]+"', '"version": "' + $Version + '"', 1)
    [System.IO.File]::WriteAllText($Path, $c, $Utf8)
    Write-Host "  patched $Path"
}
Bump-Json (Join-Path $Repo "src-tauri\tauri.conf.json")
Bump-Json (Join-Path $Repo "ui\package.json")

$cargoToml = Join-Path $Repo "src-tauri\Cargo.toml"
$c = [System.IO.File]::ReadAllText($cargoToml)
$c = [regex]::Replace($c, '(?m)^version\s*=\s*"[^"]+"', 'version = "' + $Version + '"', 1)
[System.IO.File]::WriteAllText($cargoToml, $c, $Utf8)
Write-Host "  patched $cargoToml"

# Stop a running app so target\release\vortex.exe is not locked
Get-Process -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -like "*$Repo*vortex.exe" } |
    Stop-Process -Force

Write-Host "[$now] Building frontend..." -ForegroundColor Cyan
Push-Location (Join-Path $Repo "ui")
npm run build
Pop-Location

Write-Host "[$now] Bundling NSIS installer (this runs release build + makensis)..." -ForegroundColor Cyan
Push-Location (Join-Path $Repo "src-tauri")
& "..\ui\node_modules\.bin\tauri.cmd" build
Pop-Location

$setup = Get-ChildItem (Join-Path $Repo "src-tauri\target\release\bundle\nsis") -Filter "Vortex_*_x64-setup.exe" |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1

if (-not $setup) { throw "Installer not found after build" }

$hash = (Get-FileHash $setup.FullName -Algorithm SHA256).Hash
Write-Host ""
Write-Host "Installer : $($setup.FullName)" -ForegroundColor Green
Write-Host "Size      : $([math]::Round($setup.Length / 1MB, 2)) MB"
Write-Host "SHA-256   : $hash" -ForegroundColor Green

# Browser extension packages (attach these to the GitHub Release too)
Write-Host "[$now] Building browser extensions..." -ForegroundColor Cyan
Push-Location (Join-Path $Repo "browser-extension")
node build.mjs
Pop-Location
foreach ($z in "vortex-chrome.zip", "vortex-firefox.zip") {
    $p = Join-Path $Repo "browser-extension\dist\$z"
    if (Test-Path $p) { Write-Host "Extension : $p  ($([math]::Round((Get-Item $p).Length / 1KB, 0)) KB)" -ForegroundColor Green }
}

if ($Tag) {
    git tag "v$Version"
    Write-Host "Tagged    : v$Version  (push with: git push origin v$Version)" -ForegroundColor Yellow
}
Write-Host "Done." -ForegroundColor Green