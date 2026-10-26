# Genera el instalador de Windows (NSIS).
#   powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..\src-tauri')
if (-not (Get-Command cargo-tauri -ErrorAction SilentlyContinue)) {
    cargo install tauri-cli --version '^2' --locked
}
cargo tauri build
Get-ChildItem target\release\bundle\nsis\*.exe | Select-Object FullName, Length
