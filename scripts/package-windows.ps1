# Assemble a distributable Windows zip for Cutlass (alpha packaging).
#
# Usage:
#   .\scripts\package-windows.ps1
#   .\scripts\package-windows.ps1 -NoFfmpeg   # skip DLL bundling (dev only)
#
# Output:
#   dist\Cutlass-<version>-windows-x86_64.zip
#
# Prerequisites:
#   - Rust stable (see rust-toolchain.toml)
#   - FFmpeg via vcpkg (x64-windows), with VCPKG_ROOT set
#   - release build: cargo build --release -p cutlass-ui

param(
    [switch]$NoFfmpeg,
    [ValidateSet("x86_64", "arm64")]
    [string]$Arch = "x86_64"
)

$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

$VersionLine = (Select-String -Path Cargo.toml -Pattern '^version' | Select-Object -First 1).Line
$Version = $VersionLine -replace '.*"(.*)".*', '$1'
# vcpkg triplet that matches the target architecture.
$Triplet = if ($Arch -eq "arm64") { "arm64-windows" } else { "x64-windows" }
$Dist = "dist"
$Staging = Join-Path $Dist "staging-windows-$Arch"
$Pkg = "cutlass-$Version-windows-$Arch"

Write-Host "==> packaging Cutlass $Version for Windows ($Arch)"

$BinarySrc = "target\release\cutlass-ui.exe"
if (-not (Test-Path $BinarySrc)) {
    Write-Host "==> release binary missing; building cutlass-ui"
    cargo build --release -p cutlass-ui
}

if (Test-Path $Staging) {
    Remove-Item -Recurse -Force $Staging
}
$PkgDir = Join-Path $Staging $Pkg
New-Item -ItemType Directory -Path $PkgDir -Force | Out-Null

Copy-Item $BinarySrc (Join-Path $PkgDir "cutlass-ui.exe")
Copy-Item LICENSE-MIT, LICENSE-APACHE -Destination $PkgDir

$Readme = @"
Cutlass $Version — Windows ($Arch)
===================================

Run:
  .\cutlass-ui.exe

This release bundles FFmpeg DLLs alongside the executable.

See https://github.com/1Mr-Newton/cutlass for source and issue tracker.
"@
Set-Content -Path (Join-Path $PkgDir "README-INSTALL.txt") -Value $Readme -Encoding utf8

if (-not $NoFfmpeg) {
    if (-not $env:VCPKG_ROOT) {
        throw "VCPKG_ROOT is not set; required to bundle FFmpeg DLLs (or pass -NoFfmpeg for local dev)"
    }
    $VcpkgBin = Join-Path $env:VCPKG_ROOT "installed\$Triplet\bin"
    if (-not (Test-Path $VcpkgBin)) {
        throw "vcpkg FFmpeg bin dir not found: $VcpkgBin"
    }

    Write-Host "==> bundling FFmpeg runtime DLLs from $VcpkgBin"
    Get-ChildItem (Join-Path $VcpkgBin "*.dll") |
        Where-Object { $_.Name -notmatch '^(clang|llvm|libclang)' } |
        Copy-Item -Destination $PkgDir
}

$Zip = Join-Path $Dist "Cutlass-${Version}-windows-${Arch}.zip"
if (Test-Path $Zip) {
    Remove-Item $Zip
}
Compress-Archive -Path $PkgDir -DestinationPath $Zip

$Size = (Get-Item $Zip).Length / 1MB
Write-Host ("==> wrote {0} ({1:N1} MB)" -f $Zip, $Size)
