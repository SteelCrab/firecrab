<#
.SYNOPSIS
Windows E2E: G3 capability gate, fresh install, API QA, and nested guest boot.

.DESCRIPTION
Runs on a Windows host whose `firecrab service doctor` reports ready. The gate
installs microManager with -Cli. The api, nginx, and guest phases run the shared
Linux QA scripts inside the managed distribution, which is the Firecrab host,
the same way Linux CI runs them on its own host. The caller purges afterward
with `firecrab service uninstall --purge`.

.EXAMPLE
scripts\ci-qa-windows-e2e.ps1 -Phase all -Cli target\debug\firecrab.exe
#>
[CmdletBinding()]
param(
    [ValidateSet("gate", "api", "nginx", "guest", "all")]
    [string]$Phase = "all",
    # The Windows CLI under test; the gate installs microManager with it.
    [string]$Cli = "firecrab.exe",
    # Optional Linux firecrab binary put first on the QA PATH, like macOS E2E
    # uses the checkout's CLI. Without it the release CLI in the guest runs.
    [string]$LinuxCli = "",
    # Multiplies every guest wait in the QA scripts, for hosts whose guests
    # install their first-boot packages slowly.
    [ValidateRange(1, 100)]
    [int]$WaitFactor = 1
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$distro = "firecrab-debian"
$api = "http://127.0.0.1:5523"
$qaRoot = "/root/firecrab-qa"
# `/tmp` in the distribution is a tmpfs sized from WSL's memory, firecrab-api
# may only create directories under /var/lib/firecrab, and Firecracker's API
# socket lives under the storage root, within the Unix socket path limit.
$storage = "/var/lib/firecrab/q"

function Fail([string]$Message) {
    [Console]::Error.WriteLine("FAILED G3: $Message")
    exit 1
}

function Assert-Api {
    try {
        Invoke-WebRequest -UseBasicParsing "$api/api/host" -TimeoutSec 5 | Out-Null
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
        & wsl.exe -d $distro -u root --exec bash $guestFile | Out-Host
        return $LASTEXITCODE
    } finally {
        Remove-Item -LiteralPath $file -ErrorAction SilentlyContinue
    }
}

function ConvertTo-GuestPath([string]$Path) {
    $ErrorActionPreference = "Continue"
    (& wsl.exe -d $distro -u root --exec wslpath -a $Path | Out-String).Trim()
}

function Invoke-Gate {
    $ErrorActionPreference = "Continue"
    Write-Output "G3 doctor"
    $json = & $Cli service doctor --json | Out-String
    if (-not ($json | ConvertFrom-Json).ready) {
        Write-Output $json
        Fail "WSL2 nested virtualization capability is not ready"
    }
    Write-Output "G3 install"
    & $Cli service install
    if ($LASTEXITCODE -ne 0) { Fail "service install exited with $LASTEXITCODE" }
    & $Cli service status
    if ($LASTEXITCODE -ne 0) { Fail "service status exited with $LASTEXITCODE" }
    Assert-Api
    Write-Output "PASS G3 capability, fresh install, service status, and localhost API"
}

# Copies the checkout's QA scripts into the distribution and installs the few
# tools they call that the managed guest does not ship.
function Initialize-Qa {
    $scripts = ConvertTo-GuestPath (Join-Path $root "scripts")
    $linuxCli = if ($LinuxCli) { ConvertTo-GuestPath (Resolve-Path -LiteralPath $LinuxCli).Path } else { "" }
    $setup = @'
set -eu
exec 2>&1
rm -rf '__QA__'
mkdir -p '__QA__/bin'
cp -r '__SCRIPTS__' '__QA__/scripts'
# A Windows checkout may carry CRLF endings, which bash rejects.
find '__QA__/scripts' -type f -name '*.sh' -exec sed -i 's/\r$//' {} +
if [ -n '__LINUX_CLI__' ]; then install -m 0755 '__LINUX_CLI__' '__QA__/bin/firecrab'; fi
missing=
command -v python3 >/dev/null || missing="$missing python3"
command -v ssh >/dev/null || missing="$missing openssh-client"
if [ -n "$missing" ]; then
  DEBIAN_FRONTEND=noninteractive apt-get install -y -qq $missing >/dev/null
fi
'@
    $setup = $setup.Replace("__QA__", $qaRoot).Replace("__SCRIPTS__", $scripts).Replace("__LINUX_CLI__", $linuxCli)
    if ((Invoke-InDistro $setup) -ne 0) { Fail "could not stage the QA scripts in $distro" }
}

function Invoke-Qa([string]$Script, [string]$Arguments) {
    $run = @'
exec 2>&1
cd '__QA__'
export PATH='__QA__/bin':$PATH FIRECRAB_API='__API__'
export FIRECRAB_QA_STORAGE_PATH='__STORAGE__' FIRECRAB_QA_WAIT_FACTOR='__WAIT__'
exec bash 'scripts/__SCRIPT__' __ARGS__
'@
    $run = $run.Replace("__QA__", $qaRoot).Replace("__API__", $api).Replace("__STORAGE__", $storage)
    $run = $run.Replace("__WAIT__", [string]$WaitFactor).Replace("__SCRIPT__", $Script).Replace("__ARGS__", $Arguments)
    $code = Invoke-InDistro $run
    if ($code -ne 0) {
        [Console]::Error.WriteLine("FAILED: $Script exited with $code")
        exit $code
    }
}

$guestReferences = "alpine:3.21 ubuntu:24.04 fedora:42"
switch ($Phase) {
    "gate" { Invoke-Gate }
    "api" { Assert-Api; Initialize-Qa; Invoke-Qa "ci-qa-api.sh" "" }
    "nginx" { Assert-Api; Initialize-Qa; Invoke-Qa "ci-qa-nginx.sh" "nginx:1.27-alpine" }
    "guest" { Assert-Api; Initialize-Qa; Invoke-Qa "ci-qa-guest.sh" $guestReferences }
    "all" {
        Invoke-Gate
        Initialize-Qa
        Invoke-Qa "ci-qa-api.sh" ""
        Invoke-Qa "ci-qa-nginx.sh" "nginx:1.27-alpine"
        Invoke-Qa "ci-qa-guest.sh" $guestReferences
    }
}
