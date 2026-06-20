# Download a prebuilt FFmpeg (shared + dev) for Windows and expose it via FFMPEG_DIR.
#
# This replaces the ~40-minute `vcpkg install ffmpeg` source build with a ~30-second
# download. ffmpeg-sys-next honours FFMPEG_DIR (a root containing include/, lib/, bin/)
# and prefers it over vcpkg auto-detection, so setting it is all the build needs.
#
# We pin BtbN's n7.1 LGPL "shared" builds: FFmpeg 7.1 is within ffmpeg-next 8.1's
# supported range (<= 8.0), and LGPL matches what vcpkg shipped (no GPL contamination
# of the distributed app). The "shared" archive carries headers + MSVC import libs
# (lib/*.lib) + runtime DLLs (bin/*.dll) in one folder.
#
# Usage:
#   .\scripts\fetch-ffmpeg-windows.ps1                       # x86_64 into .\ffmpeg-dev
#   .\scripts\fetch-ffmpeg-windows.ps1 -Arch arm64
#   .\scripts\fetch-ffmpeg-windows.ps1 -Dest C:\ffmpeg
#
# On GitHub Actions it appends FFMPEG_DIR to $GITHUB_ENV and bin/ to $GITHUB_PATH so
# later steps link against and can run with the bundled FFmpeg.

param(
    [ValidateSet("x86_64", "arm64")]
    [string]$Arch = "x86_64",
    [string]$Dest
)

$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
if (-not $Dest) { $Dest = Join-Path $Root "ffmpeg-dev" }

# BtbN keeps stable n7.1 assets in the rolling "latest" release.
$AssetArch = if ($Arch -eq "arm64") { "winarm64" } else { "win64" }
$Asset = "ffmpeg-n7.1-latest-$AssetArch-lgpl-shared-7.1"
$Url = "https://github.com/BtbN/FFmpeg-Builds/releases/latest/download/$Asset.zip"

$FfmpegDir = Join-Path $Dest $Asset

# The build root is the folder containing include/, lib/, bin/.
function Find-FfmpegRoot($base) {
    if (Test-Path (Join-Path $base "include")) { return $base }
    # Be resilient if BtbN ever renames the top-level folder inside the archive.
    $child = Get-ChildItem -Path $base -Directory -ErrorAction SilentlyContinue |
        Where-Object { Test-Path (Join-Path $_.FullName "include") } |
        Select-Object -First 1
    if ($child) { return $child.FullName }
    return $null
}

$found = Find-FfmpegRoot $FfmpegDir
if (-not $found) { $found = Find-FfmpegRoot $Dest }

if ($found) {
    $FfmpegDir = $found
    Write-Host "==> reusing cached FFmpeg at $FfmpegDir"
} else {
    New-Item -ItemType Directory -Path $Dest -Force | Out-Null
    $Zip = Join-Path $Dest "$Asset.zip"

    Write-Host "==> downloading prebuilt FFmpeg ($Arch) from $Url"
    Invoke-WebRequest -Uri $Url -OutFile $Zip

    Write-Host "==> extracting to $Dest"
    Expand-Archive -Path $Zip -DestinationPath $Dest -Force
    Remove-Item $Zip

    $found = Find-FfmpegRoot $FfmpegDir
    if (-not $found) { $found = Find-FfmpegRoot $Dest }
    if (-not $found) {
        throw "extracted FFmpeg has no include/ under $Dest (asset layout changed?)"
    }
    $FfmpegDir = $found
}

Write-Host "==> FFMPEG_DIR=$FfmpegDir"

if ($env:GITHUB_ENV) {
    "FFMPEG_DIR=$FfmpegDir" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
}
if ($env:GITHUB_PATH) {
    (Join-Path $FfmpegDir "bin") | Out-File -FilePath $env:GITHUB_PATH -Encoding utf8 -Append
}
