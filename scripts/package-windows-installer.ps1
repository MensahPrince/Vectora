# Build a Windows Setup.exe installer for Cutlass (Inno Setup).
#
# DORMANT: see scripts/package-windows.ps1 — Windows media backends aren't
# implemented in this line yet, so this stays a preview installer.
#
# This reuses scripts/package-windows.ps1 to build + stage the payload
# (cutlass-desktop.exe, licenses, README), then compiles it into a single
# installer via the Inno Setup compiler (ISCC.exe).
#
# Usage:
#   .\scripts\package-windows-installer.ps1
#   .\scripts\package-windows-installer.ps1 -Arch arm64        # native ARM64 build
#   .\scripts\package-windows-installer.ps1 -IsccPath "C:\...\ISCC.exe"
#
# Output:
#   dist\Cutlass-<version>-windows-<arch>-Setup.exe
#
# Prerequisites:
#   - Everything package-windows.ps1 needs (Rust stable)
#   - Inno Setup 6 (ISCC.exe on PATH or installed in Program Files,
#     or pass -IsccPath). Install via: choco install innosetup

param(
    [ValidateSet("x86_64", "arm64")]
    [string]$Arch = "x86_64",
    [string]$IsccPath
)

$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

# Stage the payload (builds the release binary if needed).
Write-Host "==> staging Windows payload via package-windows.ps1 ($Arch)"
& (Join-Path $Root "scripts\package-windows.ps1") -Arch $Arch

$VersionLine = (Select-String -Path Cargo.toml -Pattern '^version' | Select-Object -First 1).Line
$Version = $VersionLine -replace '.*"(.*)".*', '$1'
$Dist = Join-Path $Root "dist"
$SourceDir = Join-Path $Dist "staging-windows-$Arch\cutlass-$Version-windows-$Arch"

if (-not (Test-Path $SourceDir)) {
    throw "staged payload not found: $SourceDir"
}

# Locate the Inno Setup compiler.
if (-not $IsccPath) {
    $IsccCmd = Get-Command "iscc.exe" -ErrorAction SilentlyContinue
    if ($IsccCmd) { $Iscc = $IsccCmd.Source }
    if (-not $Iscc) {
        $Candidates = @(
            (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6\ISCC.exe"),
            (Join-Path $env:ProgramFiles "Inno Setup 6\ISCC.exe")
        )
        $Iscc = $Candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    }
} else {
    $Iscc = $IsccPath
}

if (-not $Iscc -or -not (Test-Path $Iscc)) {
    throw "ISCC.exe (Inno Setup 6) not found. Install with 'choco install innosetup' or pass -IsccPath."
}

Write-Host "==> compiling installer with $Iscc"
$Script = Join-Path $Root "packaging\windows\cutlass.iss"
$OutputBase = "Cutlass-$Version-windows-$Arch-Setup"
# Inno Setup architecture identifier for [Setup] ArchitecturesAllowed.
$ArchAllowed = if ($Arch -eq "arm64") { "arm64" } else { "x64compatible" }

& $Iscc `
    "/DMyAppVersion=$Version" `
    "/DMySourceDir=$SourceDir" `
    "/DMyOutputDir=$Dist" `
    "/DMyOutputBaseFilename=$OutputBase" `
    "/DMyArchAllowed=$ArchAllowed" `
    $Script

if ($LASTEXITCODE -ne 0) {
    throw "ISCC failed with exit code $LASTEXITCODE"
}

$Installer = Join-Path $Dist "$OutputBase.exe"
$Size = (Get-Item $Installer).Length / 1MB
Write-Host ("==> wrote {0} ({1:N1} MB)" -f $Installer, $Size)
