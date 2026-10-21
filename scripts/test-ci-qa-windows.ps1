<# Exercise the Windows QA runner with fake CLI/WSL transports, without a VM. #>
[CmdletBinding()]
param(
    [string]$Scenario = "",
    [string]$TestRoot = ""
)
$ErrorActionPreference = "Stop"
$runner = Join-Path $PSScriptRoot "ci-qa-windows-e2e.ps1"

if ($Scenario) {
    # Each scenario runs in a child PowerShell because the runner exits with
    # its result. Functions keep LASTEXITCODE semantics without native WSL.
    function global:wsl.exe {
        $nativeArgs = @($args)
        $global:LASTEXITCODE = 0
        if ($nativeArgs -contains "wslpath") {
            if ($Scenario -eq "path-failed") { $global:LASTEXITCODE = 12; return }
            return $nativeArgs[-1]
        }
        if ($nativeArgs -notcontains "bash") { throw "unexpected WSL call: $nativeArgs" }
        $script = Get-Content -Raw -LiteralPath $nativeArgs[-1]
        if ($script -match "npm test --prefix") {
            $progress = Get-Content -Raw -LiteralPath (Join-Path $TestRoot "results with ' spaces\summary.json") | ConvertFrom-Json
            if ($progress.status -ne 'RUNNING' -or $null -ne $progress.exitCode) { throw "in-progress QA was reported as complete" }
            if (@($progress.results | Where-Object { $_.phase -eq 'browser' -and $_.status -eq 'RUNNING' }).Count -ne 1) { throw "browser progress was not retained" }
        }
        Add-Content -LiteralPath (Join-Path $TestRoot "wsl-scripts.log") -Value $script
        if ($script -match "scripts/ci-qa-api.sh" -and $Scenario -eq "all-failed") { $global:LASTEXITCODE = 17 }
        if ($script -match "scripts/ci-qa-guest.sh" -and $Scenario -eq "all-failed") { $global:LASTEXITCODE = 23 }
        if ($script -match "scripts/ci-qa-lifetime.sh" -and $Scenario -eq "lifetime-failed") { $global:LASTEXITCODE = 29 }
        if ($script -match "npm test --prefix" -and $Scenario -eq "browser-failed") { $global:LASTEXITCODE = 42 }
        Write-Output "fake WSL exit=$global:LASTEXITCODE"
    }
    function global:Invoke-WebRequest {
        [CmdletBinding()]
        param([string]$Uri, [switch]$UseBasicParsing, [int]$TimeoutSec)
        if ($Scenario -eq "api-failed") { Write-Error "API is unreachable"; return }
        @{ StatusCode = 200 }
    }
    $env:FIRECRAB_QA_TEST_SCENARIO = $Scenario
    $env:FIRECRAB_QA_TEST_ROOT = $TestRoot
    $options = @{
        Phase = if ($Scenario -in @("all-failed", "all-passed", "all-shell-failed")) { "all" } elseif ($Scenario -like "shell-*") { "shell" } elseif ($Scenario -eq "lifetime-failed") { "lifetime" } else { "browser" }
        Cli = Join-Path $TestRoot "cli.ps1"
        ResultsDir = Join-Path $TestRoot "results with ' spaces"
    }
    if ($Scenario -in @("all-failed", "gate-failed", "doctor-crashed", "api-failed")) { $options.Source = $TestRoot }
    if ($Scenario -in @("all-failed", "release-invalid")) { $options.Release = $true }
    & $runner @options
    exit $LASTEXITCODE
}

function Assert([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

$temporary = Join-Path ([IO.Path]::GetTempPath()) ("firecrab-qa-contract-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $temporary | Out-Null
try {
    $shell = (Get-Process -Id $PID).Path
    $passed = 0
    foreach ($case in @("all-failed", "all-passed", "all-shell-failed", "shell-passed", "shell-failed", "shell-state-failed", "shell-arguments-failed", "shell-output-failed", "shell-exit-failed", "lifetime-failed", "browser-failed", "gate-failed", "doctor-crashed", "api-failed", "path-failed", "release-invalid")) {
        $caseRoot = Join-Path $temporary $case
        New-Item -ItemType Directory -Path $caseRoot | Out-Null
        $fakeCli = @'
$arguments = @($args)
$global:LASTEXITCODE = 0
Add-Content -LiteralPath (Join-Path $env:FIRECRAB_QA_TEST_ROOT 'cli-calls.log') -Value ($arguments -join ' ')
if ($arguments -contains 'shell') {
    if ($arguments -contains 'id') {
        if ($env:FIRECRAB_QA_TEST_SCENARIO -in @('shell-failed', 'all-shell-failed')) { Write-Output 'wrong-user' } else { Write-Output 'root' }
    } elseif ($arguments -contains 'systemctl') {
        Write-Output 'active'
        if ($env:FIRECRAB_QA_TEST_SCENARIO -eq 'shell-state-failed') { $global:LASTEXITCODE = 4 }
    } elseif ($arguments -contains 'printf') {
        if ($env:FIRECRAB_QA_TEST_SCENARIO -eq 'shell-arguments-failed') {
            Write-Output 'arguments changed'
        } else {
            Write-Output "[it's here]`n[a b]`n[`$HOME]"
            if ($env:FIRECRAB_QA_TEST_SCENARIO -eq 'shell-output-failed') { $global:LASTEXITCODE = 13 }
        }
    } elseif ($arguments -contains 'sh') {
        $global:LASTEXITCODE = if ($env:FIRECRAB_QA_TEST_SCENARIO -eq 'shell-exit-failed') { 0 } else { 7 }
    } else { throw 'unexpected service shell command' }
}
if ($arguments -contains 'doctor') {
    if ($env:FIRECRAB_QA_TEST_SCENARIO -eq 'gate-failed') {
        $global:LASTEXITCODE = 1
        Write-Output '{"platform":"Windows","ready":false,"checks":[]}'
    } else {
        if ($env:FIRECRAB_QA_TEST_SCENARIO -eq 'doctor-crashed') { $global:LASTEXITCODE = 7 }
        Write-Output '{"platform":"Windows","ready":true,"checks":[]}'
    }
}
'@
        Set-Content -LiteralPath (Join-Path $caseRoot "cli.ps1") -Value $fakeCli -Encoding UTF8
        $ErrorActionPreference = "Continue"
        $output = & $shell -NoProfile -ExecutionPolicy Bypass -File $PSCommandPath -Scenario $case -TestRoot $caseRoot 2>&1 | Out-String
        $code = $LASTEXITCODE
        $ErrorActionPreference = "Stop"
        $directory = Join-Path $caseRoot "results with ' spaces"
        $summaryPath = Join-Path $directory "summary.json"
        Assert (Test-Path -LiteralPath $summaryPath) "$case did not retain a summary: $output"
        Assert (Test-Path -LiteralPath (Join-Path $directory "run.log")) "$case did not retain a transcript"
        $summary = Get-Content -Raw -LiteralPath $summaryPath | ConvertFrom-Json
        Assert ($summary.exitCode -eq $code) "$case summary lost the process exit code: $output"
        Assert ($summary.status -ne 'RUNNING') "$case summary was never finalized"
        if ($case -eq "all-failed") {
            Assert ($code -eq 1) "all did not fail after failed phases: $output"
            $phases = @($summary.results | Where-Object { $_.phase -in @("api", "nginx", "guest", "browser") })
            Assert (($phases.phase -join ',') -eq 'api,nginx,guest,browser') "all failed to run every QA phase: $output"
            Assert ($phases[0].exitCode -eq 17 -and $phases[2].exitCode -eq 23) "all lost individual failure codes"
            Assert ($phases[1].status -eq 'PASS' -and $phases[3].status -eq 'PASS') "later passing phases were lost"
            Assert (@($summary.results | Where-Object { $_.phase -eq 'shell' -and $_.status -eq 'PASS' }).Count -eq 1) "all lost the G6 shell phase"
            $calls = Get-Content -Raw -LiteralPath (Join-Path $caseRoot 'cli-calls.log')
            Assert ($calls -match 'service dev --source.*--yes --release') "source release deployment was not requested"
        } elseif ($case -eq "all-passed") {
            Assert ($code -eq 0) "all successful phases did not produce exit 0: $output"
            Assert (@($summary.results | Where-Object { $_.status -ne 'PASS' }).Count -eq 0) "successful run has an incorrect phase verdict"
            Assert (@($summary.results | Where-Object { $_.phase -in @('shell', 'api', 'nginx', 'guest', 'lifetime', 'browser') }).Count -eq 6) "successful run did not cover all phases"
        } elseif ($case -eq 'all-shell-failed' -or $case -like 'shell-*') {
            $expectedStatus = if ($case -eq 'shell-passed') { 'PASS' } else { 'FAILED' }
            $expectedCode = if ($case -eq 'shell-passed') { 0 } else { 1 }
            Assert ($code -eq $expectedCode) "$case lost the shell result: $output"
            Assert (@($summary.results | Where-Object { $_.phase -eq 'shell' -and $_.status -eq $expectedStatus }).Count -eq 1) "$case did not retain the shell verdict"
            if ($case -eq 'all-shell-failed') {
                Assert (@($summary.results | Where-Object { $_.phase -in @('api', 'nginx', 'guest', 'lifetime', 'browser') -and $_.status -eq 'PASS' }).Count -eq 5) "shell failure suppressed later phases: $output"
            }
        } elseif ($case -eq 'lifetime-failed') {
            Assert ($code -eq 29) "single lifetime phase lost exit 29: $output"
            Assert (@($summary.results | Where-Object { $_.phase -eq 'lifetime' -and $_.status -eq 'FAILED' -and $_.exitCode -eq 29 }).Count -eq 1) "lifetime failure was not retained"
        } elseif ($case -eq "browser-failed") {
            Assert ($code -eq 42) "single browser phase lost exit 42: $output"
            Assert (@($summary.results | Where-Object { $_.phase -eq 'artifacts' }).Count -eq 1) "failed browser did not collect artifacts"
            $scripts = Get-Content -Raw -LiteralPath (Join-Path $caseRoot 'wsl-scripts.log')
            Assert ($scripts -match 'FIRECRAB_E2E_REQUIRE_GUEST_BOOT=1') "browser did not require guest boot"
            Assert ($scripts -match 'FIRECRAB_E2E_REQUIRE_RUNNING_API=1') "browser did not require the managed API"
            Assert ($scripts -match 'unset FIRECRAB_E2E_SKIP_GUEST_BOOT') "browser inherited an import-only mode"
            Assert ($scripts -match ([regex]::Escape("results with '\'' spaces"))) "artifact path apostrophe was not shell-quoted"
        } else {
            Assert ($code -ne 0) "$case unexpectedly passed: $output"
            Assert (@($summary.results | Where-Object { $_.phase -eq 'browser' -and $_.status -eq 'WARNING' }).Count -eq 1) "$case did not record the unrun browser phase"
            if ($case -eq 'api-failed') {
                Assert (@($summary.results | Where-Object { $_.phase -eq 'gate' -and $_.status -eq 'FAILED' }).Count -eq 1) "unreachable API passed the gate"
            }
        }
        Write-Output "PASS $case"
        $passed++
    }
    Write-Output "Windows QA runner: $passed passed"
} finally {
    Remove-Item -LiteralPath $temporary -Recurse -Force -ErrorAction SilentlyContinue
}
