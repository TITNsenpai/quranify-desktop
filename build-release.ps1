# Builds dist\Quranify.exe and dist\Quranify-Setup.exe (the installer embeds the app).
# Usage:  powershell -ExecutionPolicy Bypass -File .\build-release.ps1
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

# A short target folder avoids Windows' 260-character path limit during linking.
$env:CARGO_TARGET_DIR = Join-Path $env:USERPROFILE '.cargo\qd-target'
$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargo) { $cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }

New-Item -ItemType Directory -Force dist | Out-Null

& $cargo build --release --bin quranify-desktop
if ($LASTEXITCODE) { throw 'app build failed' }
Copy-Item (Join-Path $env:CARGO_TARGET_DIR 'release\quranify-desktop.exe') 'dist\Quranify.exe' -Force

& $cargo build --release --bin setup
if ($LASTEXITCODE) { throw 'installer build failed' }
Copy-Item (Join-Path $env:CARGO_TARGET_DIR 'release\setup.exe') 'dist\Quranify-Setup.exe' -Force

Get-Item dist\Quranify.exe, dist\Quranify-Setup.exe | Select-Object Name, @{n='MB'; e={[math]::Round($_.Length / 1MB, 2)}}
