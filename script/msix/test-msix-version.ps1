# Tests script\msix\msix-version.ps1 (the MSIX version of Solo's version).
#
#   powershell -ExecutionPolicy Bypass -File script\msix\test-msix-version.ps1
#
# Exits with 1 if a test fails.

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "msix-version.ps1")

$failures = 0
function Test-Equal($name, $actual, $expected) {
    if ($actual -ne $expected) {
        Write-Host "FAIL ${name}: got '$actual', expected '$expected'"
        $script:failures++
    }
}
function Test-Throws($name, [scriptblock]$block) {
    try {
        & $block | Out-Null
        Write-Host "FAIL ${name}: didn't throw"
        $script:failures++
    } catch {}
}

Test-Equal "release" (ConvertTo-MsixVersion "2.3.0") "2.3.0.0"
Test-Equal "spaces" (ConvertTo-MsixVersion " 2.3.1`n") "2.3.1.0"
Test-Equal "build metadata" (ConvertTo-MsixVersion "2.3.0+stable.12.abc") "2.3.0.0"
Test-Equal "largest" (ConvertTo-MsixVersion "65535.65535.65535") "65535.65535.65535.0"
Test-Equal "zero major" (ConvertTo-MsixVersion "0.1.0") "0.1.0.0"
Test-Equal "prerelease allowed" (ConvertTo-MsixVersion "2.4.0-beta.1" -AllowPrerelease) "2.4.0.0"
Test-Throws "prerelease" { ConvertTo-MsixVersion "2.4.0-beta.1" }
Test-Throws "two parts" { ConvertTo-MsixVersion "2.3" }
Test-Throws "four parts" { ConvertTo-MsixVersion "2.3.0.1" }
Test-Throws "too large" { ConvertTo-MsixVersion "2.65536.0" }
Test-Throws "all zero" { ConvertTo-MsixVersion "0.0.0" }
Test-Throws "not a number" { ConvertTo-MsixVersion "2.x.0" }
Test-Throws "empty" { ConvertTo-MsixVersion "" }

Assert-MsixVersion "2.3.0.0" -Store
Assert-MsixVersion "2.3.0.7"
Test-Throws "store revision" { Assert-MsixVersion "2.3.0.7" -Store }
Test-Throws "three parts" { Assert-MsixVersion "2.3.0" }
Test-Throws "too large part" { Assert-MsixVersion "2.3.0.70000" }

if ($failures) {
    Write-Host "$failures test(s) failed"
    exit 1
}
Write-Host "All MSIX version tests passed"
exit 0
