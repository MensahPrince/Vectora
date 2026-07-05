# Assemble a distributable Windows zip for Cutlass (alpha packaging).
#
# DORMANT: the app compiles and launches on Windows, but this branch's media
# stack (decode/encode) has no Windows backend yet — imported media won't
# play. No FFmpeg DLLs are bundled (the engine doesn't link FFmpeg); the
# script is kept working so packaging is ready when the backend lands.
#
# Usage:
#   .\scripts\package-windows.ps1
#   .\scripts\package-windows.ps1 -Arch arm64
#
# Output:
#   dist\Cutlass-<version>-windows-<arch>.zip
#
# Prerequisites:
#   - Rust stable (see rust-toolchain.toml)
#   - release build: cargo build --release -p cutlass-desktop

param(
    [ValidateSet("x86_64", "arm64")]
    [string]$Arch = "x86_64"
)

$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

$VersionLine = (Select-String -Path Cargo.toml -Pattern '^version' | Select-Object -First 1).Line
$Version = $VersionLine -replace '.*"(.*)".*', '$1'
$Dist = "dist"
$Staging = Join-Path $Dist "staging-windows-$Arch"
$Pkg = "cutlass-$Version-windows-$Arch"

Write-Host "==> packaging Cutlass $Version for Windows ($Arch)"

$BinarySrc = "target\release\cutlass-desktop.exe"
if (-not (Test-Path $BinarySrc)) {
    Write-Host "==> release binary missing; building cutlass-desktop"
    cargo build --release -p cutlass-desktop
}

if (Test-Path $Staging) {
    Remove-Item -Recurse -Force $Staging
}
$PkgDir = Join-Path $Staging $Pkg
New-Item -ItemType Directory -Path $PkgDir -Force | Out-Null

Copy-Item $BinarySrc (Join-Path $PkgDir "cutlass-desktop.exe")
Copy-Item LICENSE-MIT, LICENSE-APACHE -Destination $PkgDir

$Readme = @"
Cutlass $Version — Windows ($Arch)
===================================

Run:
  .\cutlass-desktop.exe

Preview build: the editor UI runs, but video/audio decode and export are
not implemented on Windows yet in this line — imported media will not play.

See https://github.com/1Mr-Newton/cutlass for source and issue tracker.
"@
Set-Content -Path (Join-Path $PkgDir "README-INSTALL.txt") -Value $Readme -Encoding utf8

$Zip = Join-Path $Dist "Cutlass-${Version}-windows-${Arch}.zip"
if (Test-Path $Zip) {
    Remove-Item $Zip
}
Compress-Archive -Path $PkgDir -DestinationPath $Zip

$Size = (Get-Item $Zip).Length / 1MB
Write-Host ("==> wrote {0} ({1:N1} MB)" -f $Zip, $Size)
