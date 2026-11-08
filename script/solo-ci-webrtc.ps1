# Downloads the prebuilt libwebrtc that livekit's webrtc-sys links, checks its
# SHA-256, and unpacks it to <Dir>\win-x64-release, for LK_CUSTOM_WEBRTC. Used
# by .github/workflows/solo-ci.yml and solo-package.yml; locally it does the
# same as fetching the zip by hand.
#
#   script\solo-ci-webrtc.ps1 -Tag webrtc-0001d84-4 -Sha256 <hash> -Dir <dir>
#
# The tag must match WEBRTC_TAG in the livekit-rust-sdks revision pinned in
# Cargo.toml; the hash is the one GitHub shows for the release asset.

param(
    [Parameter(Mandatory = $true)][string]$Tag,
    [Parameter(Mandatory = $true)][string]$Sha256,
    [Parameter(Mandatory = $true)][string]$Dir,
    [string]$Triple = "win-x64-release"
)
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$url = "https://github.com/zed-industries/livekit-rust-sdks/releases/download/$Tag/webrtc-$Triple.zip"
New-Item -ItemType Directory -Force $Dir | Out-Null
$zip = Join-Path $Dir "webrtc-$Triple.zip"

Write-Host "Downloading $url"
Invoke-WebRequest -Uri $url -OutFile $zip
$actual = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant()
if ($actual -ne $Sha256.ToLowerInvariant()) {
    throw "SHA-256 of $url is $actual, expected $Sha256"
}

Expand-Archive -Path $zip -DestinationPath $Dir -Force
Remove-Item -Force $zip
if (-not (Test-Path (Join-Path $Dir "$Triple\webrtc.ninja"))) {
    throw "The WebRTC archive didn't unpack to $Dir\$Triple"
}
Write-Host "WebRTC $Tag is in $(Join-Path $Dir $Triple)"
