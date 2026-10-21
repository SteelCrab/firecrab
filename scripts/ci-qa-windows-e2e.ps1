<#
.SYNOPSIS
Windows E2E: G3 capability gate, source deployment, G6 service shell, browser/API QA, and nested guest boot.

.DESCRIPTION
Runs on a Windows host whose `firecrab service doctor` reports ready. The gate
installs microManager with -Cli. With -Source, it then builds and deploys that
checkout using service dev before running QA. The shell phase checks G6 from Windows. The api, nginx, and guest phases run the shared
Linux QA scripts inside the managed distribution, which is the Firecrab host,
the same way Linux CI runs them on its own host. After successful prerequisites,
all runs every test phase and returns failure if any fails. Results and browser
artifacts are retained on Windows. Test-created resources are cleaned up by those
scripts; the existing distribution and user data are preserved.

.EXAMPLE
scripts\ci-qa-windows-e2e.ps1 -Phase all -Cli target\debug\firecrab.exe

.EXAMPLE
scripts\ci-qa-windows-e2e.ps1 -Phase all -Cli target\debug\firecrab.exe -Source .
#>
[CmdletBinding()]
param(
    [ValidateSet("gate", "shell", "browser", "api", "nginx", "guest", "lifetime", "all")]
    [string]$Phase = "all",
    # The Windows CLI under test; the gate installs microManager with it.
    [string]$Cli = "firecrab.exe",
    # Deploy API + net-helper from this checkout instead of testing the pinned release.
    [string]$Source = "",
    [switch]$Release,
    # Optional Linux firecrab binary put first on the QA PATH, like macOS E2E
    # uses the checkout's CLI. Without it the release CLI in the guest runs.
    [string]$LinuxCli = "",
    # Multiplies nginx/SSH waits, not package-install or browser deadlines.
    [ValidateRange(1, 100)]
    [int]$WaitFactor = 1,
    # Retain the transcript, phase summary, and browser traces on Windows.
    [string]$ResultsDir = ""
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$distro = "firecrab-debian"
$api = "http://127.0.0.1:5523"
$runId = (Get-Date -Format "yyyyMMdd-HHmmss") + "-" + [guid]::NewGuid().ToString("N").Substring(0, 8)
$qaRoot = "/root/firecrab-qa/$runId"
if (-not $ResultsDir) { $ResultsDir = Join-Path $root "target\qa\windows\$runId" }
$ResultsDir = [IO.Path]::GetFullPath($ResultsDir)
New-Item -ItemType Directory -Force -Path $ResultsDir | Out-Null
$results = New-Object 'Collections.Generic.List[object]'
$overallExit = 0
$phaseSucceeded = $false
$runCompleted = $false
# `/tmp` in the distribution is a tmpfs sized from WSL's memory, firecrab-api
# may only create directories under /var/lib/firecrab, and Firecracker's API
# socket lives under the storage root, within the Unix socket path limit.
$storage = "/var/lib/firecrab/q"

function Fail([string]$Message, [int]$Code = 1) {
    $failure = [Exception]::new($Message)
    $failure.Data["exitCode"] = $Code
    throw $failure
}

function Quote-Shell([string]$Value) {
    "'" + $Value.Replace("'", "'" + [char]92 + "''") + "'"
}

function Save-Summary {
    [ordered]@{
        schemaVersion = 1
        phase = $Phase
        cli = $Cli
        source = $Source
        release = [bool]$Release
        waitFactor = $WaitFactor
        guestResultsPath = $qaRoot
        status = if (-not $script:runCompleted) { "RUNNING" } elseif ($script:overallExit -eq 0) { "PASS" } else { "FAILED" }
        exitCode = if ($script:runCompleted) { $script:overallExit } else { $null }
        results = @($results.ToArray())
    } | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $ResultsDir "summary.json") -Encoding UTF8
}

function Invoke-QaPhase([string]$Name, [scriptblock]$Action) {
    $entry = [ordered]@{ phase = $Name; status = "RUNNING"; exitCode = $null; detail = "" }
    $results.Add($entry)
    $script:phaseSucceeded = $false
    Save-Summary
    try {
        & $Action | Out-Host
        $entry.status = "PASS"
        $entry.exitCode = 0
        $script:phaseSucceeded = $true
    } catch {
        $entry.status = "FAILED"
        $entry.exitCode = 1
        $entry.detail = $_.Exception.Message
        if ($_.Exception.Data.Contains("exitCode")) { $entry.exitCode = [int]$_.Exception.Data["exitCode"] }
        if ($script:overallExit -eq 0) {
            $script:overallExit = if ($Phase -eq "all") { 1 } else { $entry.exitCode }
        }
        [Console]::Error.WriteLine("FAILED ${Name}: $($entry.detail)")
    }
    Save-Summary
}

function Assert-Api {
    try {
        Invoke-WebRequest -UseBasicParsing "$api/api/host" -TimeoutSec 5 -ErrorAction Stop | Out-Null
    } catch {
        Fail "management API is not reachable at $api; run the gate phase first"
    }
}

# Windows PowerShell turns a native program's stderr into error records once
# the output is redirected, and "Stop" would abort on the first such line, so
# every function that runs wsl.exe or the CLI relaxes it for its own scope.

# PowerShell 5.1 mangles double quotes in native arguments, so a script goes
# to the distribution as an LF-only file rather than as a `bash -c` argument.
function Invoke-InDistro([string]$Script) {
    $ErrorActionPreference = "Continue"
    $file = Join-Path ([IO.Path]::GetTempPath()) ("firecrab-qa-" + [guid]::NewGuid() + ".sh")
    [IO.File]::WriteAllText($file, ($Script -replace "`r`n", "`n"), (New-Object Text.UTF8Encoding $false))
    try {
        $guestFile = (& wsl.exe -d $distro -u root --exec wslpath -a $file | Out-String).Trim()
        if ($LASTEXITCODE -ne 0 -or -not $guestFile) { Fail "wslpath could not translate the QA script path" }
        & wsl.exe -d $distro -u root --exec bash $guestFile | Out-Host
        return $LASTEXITCODE
    } finally {
        Remove-Item -LiteralPath $file -ErrorAction SilentlyContinue
    }
}

function ConvertTo-GuestPath([string]$Path) {
    $ErrorActionPreference = "Continue"
    $translated = (& wsl.exe -d $distro -u root --exec wslpath -a $Path | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or -not $translated) { Fail "wslpath could not translate $Path" }
    $translated
}

function Invoke-Gate {
    $ErrorActionPreference = "Continue"
    Write-Output "G3 doctor"
    $json = & $Cli service doctor --json | Out-String
    if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 1) { Fail "service doctor exited with $LASTEXITCODE" }
    if (-not ($json | ConvertFrom-Json).ready) {
        Write-Output $json
        Fail "WSL2 nested virtualization capability is not ready"
    }
    Write-Output "G3 install"
    & $Cli service install --yes
    if ($LASTEXITCODE -ne 0) { Fail "service install exited with $LASTEXITCODE" }
    if ($Source) {
        Write-Output "G3 deploy source checkout"
        $devArgs = @("service", "dev", "--source", (Resolve-Path -LiteralPath $Source).Path, "--yes")
        if ($Release) { $devArgs += "--release" }
        & $Cli @devArgs
        if ($LASTEXITCODE -ne 0) { Fail "service dev exited with $LASTEXITCODE" }
    }
    & $Cli service status
    if ($LASTEXITCODE -ne 0) { Fail "service status exited with $LASTEXITCODE" }
    Assert-Api
    Write-Output "PASS G3 capability, install, service status, and localhost API"
}

# G6: `service shell` runs a command in the distribution as root, passes each
# argument through unchanged, and returns the command's exit code. The CLI
# takes every word after `shell` as the command.
function Invoke-Shell {
    $ErrorActionPreference = "Continue"
    $user = (& $Cli service shell id -un | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $user -ne "root") { Fail "commands run as '$user' (exit $LASTEXITCODE), expected root" }
    $state = (& $Cli service shell systemctl is-active firecrab-api | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $state -ne "active") { Fail "firecrab-api is '$state' in $distro, expected active" }
    $output = ((& $Cli service shell printf '[%s]\n' "it's here" 'a b' '$HOME' | Out-String) -replace "`r", "").TrimEnd([char]10)
    if ($LASTEXITCODE -ne 0 -or $output -ne "[it's here]`n[a b]`n[`$HOME]") { Fail "arguments arrived as: $output (exit $LASTEXITCODE)" }
    & $Cli service shell sh -c 'exit 7' | Out-Null
    if ($LASTEXITCODE -ne 7) { Fail "a command that exits 7 returned $LASTEXITCODE" }
    Write-Output "PASS G6 service shell runs as root, keeps arguments, and returns exit codes"
}

# Copies the checkout's QA scripts into the distribution and installs the few
# tools they call that the managed guest does not ship.
function Initialize-Qa {
    $scripts = ConvertTo-GuestPath (Join-Path $root "scripts")
    $linuxCli = if ($LinuxCli) { ConvertTo-GuestPath (Resolve-Path -LiteralPath $LinuxCli).Path } else { "" }
    $setup = @'
set -eu
exec 2>&1
mkdir -p __QA__/bin
cp -r __SCRIPTS__ __QA__/scripts
# A Windows checkout may carry CRLF endings, which bash rejects.
find __QA__/scripts -type f -name '*.sh' -exec sed -i 's/\r$//' {} +
if [ -n __LINUX_CLI__ ]; then install -m 0755 __LINUX_CLI__ __QA__/bin/firecrab; fi
missing=
command -v python3 >/dev/null || missing="$missing python3"
command -v ssh >/dev/null || missing="$missing openssh-client"
if [ -n "$missing" ]; then
  DEBIAN_FRONTEND=noninteractive apt-get install -y -qq $missing >/dev/null
fi
'@
    $setup = $setup.Replace("__QA__", (Quote-Shell $qaRoot)).Replace("__SCRIPTS__", (Quote-Shell $scripts)).Replace("__LINUX_CLI__", (Quote-Shell $linuxCli))
    if ((Invoke-InDistro $setup) -ne 0) { Fail "could not stage the QA scripts in $distro" }
}

function Invoke-Qa([string]$Script, [string]$Arguments) {
    $powershell = ConvertTo-GuestPath (Join-Path $env:SystemRoot "System32\WindowsPowerShell\v1.0\powershell.exe")
    $run = @'
set -eu
exec 2>&1
cd __QA__
export PATH=__QA__/bin:$PATH FIRECRAB_API='__API__'
export FIRECRAB_QA_STORAGE_PATH='__STORAGE__' FIRECRAB_QA_WAIT_FACTOR='__WAIT__'
export FIRECRAB_QA_WINDOWS_POWERSHELL=__POWERSHELL__
exec bash 'scripts/__SCRIPT__' __ARGS__
'@
    $run = $run.Replace("__QA__", (Quote-Shell $qaRoot)).Replace("__API__", $api).Replace("__STORAGE__", $storage)
    $run = $run.Replace("__WAIT__", [string]$WaitFactor).Replace("__SCRIPT__", $Script).Replace("__ARGS__", $Arguments)
    $run = $run.Replace("__POWERSHELL__", (Quote-Shell $powershell))
    $code = Invoke-InDistro $run
    if ($code -ne 0) {
        Fail "$Script exited with $code" $code
    }
}

# The browser and local OCI fixture share the API's Linux loopback, including
# direct IPv6 SSH to the workload. Windows node_modules cannot be reused here.
function Invoke-BrowserQa {
    $checkout = ConvertTo-GuestPath $root
    $run = @'
set -euo pipefail
exec 2>&1
cd __QA__
tar -C __CHECKOUT__ --exclude=node_modules --exclude=test-results --exclude=playwright-report \
  -cf - firecrab-e2e firecrab-frontend | tar -xf -
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends nodejs npm openssh-server
npm ci --prefix firecrab-e2e
npm ci --prefix firecrab-frontend
./firecrab-e2e/node_modules/.bin/playwright install --with-deps chromium
export FIRECRAB_E2E_REUSE_SERVER=1
export FIRECRAB_E2E_REQUIRE_GUEST_BOOT=1
export FIRECRAB_E2E_REQUIRE_RUNNING_API=1
unset FIRECRAB_E2E_SKIP_GUEST_BOOT FIRECRAB_QA_MANAGER_HOST FIRECRAB_QA_MANAGER_KEY
npm test --prefix firecrab-e2e
'@
    $run = $run.Replace("__QA__", (Quote-Shell $qaRoot)).Replace("__CHECKOUT__", (Quote-Shell $checkout))
    $code = Invoke-InDistro $run
    if ($code -ne 0) { Fail "browser E2E exited with $code" $code }
}

function Save-BrowserResults {
    $destination = ConvertTo-GuestPath (Join-Path $ResultsDir "browser-results.tar.gz")
    $save = @'
set -euo pipefail
cd __QA__
files=()
for path in firecrab-e2e/test-results firecrab-e2e/playwright-report; do
    if [ -d "$path" ]; then files+=("$path"); fi
done
if [ "${#files[@]}" -gt 0 ]; then tar -czf __DESTINATION__ "${files[@]}"; fi
'@
    $save = $save.Replace("__QA__", (Quote-Shell $qaRoot)).Replace("__DESTINATION__", (Quote-Shell $destination))
    if ((Invoke-InDistro $save) -ne 0) { Fail "could not retain browser results on Windows" }
}

$staged = $false
Start-Transcript -LiteralPath (Join-Path $ResultsDir "run.log") | Out-Null
try {
    if ($Release -and -not $Source) { Fail "-Release requires -Source" }
    if ($Source -or $Phase -eq "gate" -or $Phase -eq "all") {
        Invoke-QaPhase "gate" { Invoke-Gate }
        if (-not $phaseSucceeded) { Fail "runtime gate failed; QA phases were not run" $overallExit }
    }
    if ($Phase -ne "gate") {
        Invoke-QaPhase "setup" { Assert-Api; Initialize-Qa }
        if (-not $phaseSucceeded) { Fail "QA setup failed; test phases were not run" $overallExit }
        $staged = $true
        $phases = if ($Phase -eq "all") { @("shell", "api", "nginx", "guest", "lifetime", "browser") } else { @($Phase) }
        foreach ($item in $phases) {
            switch ($item) {
                "shell" { Invoke-QaPhase "shell" { Invoke-Shell } }
                "api" { Invoke-QaPhase "api" { Invoke-Qa "ci-qa-api.sh" "" } }
                "nginx" { Invoke-QaPhase "nginx" { Invoke-Qa "ci-qa-nginx.sh" "nginx:1.27-alpine" } }
                "guest" { Invoke-QaPhase "guest" { Invoke-Qa "ci-qa-guest.sh" "alpine:3.21 ubuntu:24.04 fedora:42" } }
                "lifetime" { Invoke-QaPhase "lifetime" { Invoke-Qa "ci-qa-lifetime.sh" "alpine:3.21" } }
                "browser" { Invoke-QaPhase "browser" { Invoke-BrowserQa } }
            }
        }
    }
} catch {
    if ($overallExit -eq 0) {
        $overallExit = 1
        $results.Add([ordered]@{ phase = "setup"; status = "FAILED"; exitCode = 1; detail = $_.Exception.Message })
    }
    [Console]::Error.WriteLine("FAILED: $($_.Exception.Message)")
} finally {
    $expectedPhases = if ($Phase -eq "all") { @("shell", "api", "nginx", "guest", "lifetime", "browser") } else { @($Phase) }
    foreach ($expected in $expectedPhases) {
        if (@($results | Where-Object { $_.phase -eq $expected }).Count -eq 0) {
            $results.Add([ordered]@{ phase = $expected; status = "WARNING"; exitCode = $null; detail = "not run: prerequisite failed" })
            if ($overallExit -eq 0) { $overallExit = 1 }
        }
    }
    foreach ($entry in $results) {
        if ($entry.status -eq "RUNNING") {
            $entry.status = "WARNING"
            $entry.detail = "interrupted before phase completion"
            if ($overallExit -eq 0) { $overallExit = 1 }
        }
    }
    if ($staged) { Invoke-QaPhase "artifacts" { Save-BrowserResults } }
    $runCompleted = $true
    Save-Summary
    Stop-Transcript | Out-Null
    Write-Output "QA results: $ResultsDir"
}
exit $overallExit
