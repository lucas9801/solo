# MSIX package versions for Solo, dot-sourced by script\package-solo-msix.ps1
# (and script\msix\test-msix-version.ps1, which tests them).
#
# An MSIX version has four parts, each 0-65535, and the Microsoft Store wants
# the last one to be 0 (it's reserved for the Store). Solo's version x.y.z
# (crates/zed/Cargo.toml, also each exe's product version) becomes x.y.z.0.

# Solo's version as an MSIX version: "2.3.0" -> "2.3.0.0". Build metadata
# ("+stable.12") is dropped. A prerelease ("2.4.0-beta.1") has no MSIX
# equivalent that sorts before 2.4.0.0, so it's refused unless
# -AllowPrerelease (test packages), which drops it.
function ConvertTo-MsixVersion([string]$SoloVersion, [switch]$AllowPrerelease) {
    $core = $SoloVersion.Trim() -replace '\+.*$', ''
    if ($core -match '-') {
        if (-not $AllowPrerelease) {
            throw "Solo $SoloVersion is a prerelease, which an MSIX version can't express; the Store only takes releases"
        }
        $core = $core -replace '-.*$', ''
    }
    if ($core -notmatch '^\d+\.\d+\.\d+$') { throw "Solo's version must look like 2.3.0, not '$SoloVersion'" }
    $version = "$core.0"
    Assert-MsixVersion $version
    return $version
}

# Throws unless $Version is a valid MSIX version: four parts, each 0-65535,
# not 0.0.0.0; with -Store, the last part is 0 as well.
function Assert-MsixVersion([string]$Version, [switch]$Store) {
    if ($Version -notmatch '^\d+\.\d+\.\d+\.\d+$') { throw "An MSIX version looks like 2.3.0.0, not '$Version'" }
    $parts = $Version.Split('.') | ForEach-Object { [long]$_ }
    if ($parts | Where-Object { $_ -gt 65535 }) { throw "Each part of an MSIX version is at most 65535: '$Version'" }
    if (($parts | Measure-Object -Sum).Sum -eq 0) { throw "An MSIX version can't be 0.0.0.0" }
    if ($Store -and $parts[3] -ne 0) { throw "The Store wants the last part of the version to be 0: '$Version'" }
}
