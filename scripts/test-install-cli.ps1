$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$installer = Join-Path $root "install-cli.ps1"

function Restore-ProcessEnvironment {
    param([string]$Name, [AllowNull()][string]$Value)

    if ($null -eq $Value) {
        Remove-Item -LiteralPath "Env:$Name" -ErrorAction SilentlyContinue
    } else {
        Set-Item -LiteralPath "Env:$Name" -Value $Value
    }
}

$oldArch = $env:FIRECRAB_CLI_ARCH
$oldReleaseBase = $env:FIRECRAB_RELEASE_BASE
$oldAllowFileUrl = $env:FIRECRAB_TEST_ALLOW_FILE_URL
$oldProcessPath = $env:Path
$oldUserPath = [Environment]::GetEnvironmentVariable("Path", "User")
$scratch = Join-Path ([System.IO.Path]::GetTempPath()) ("firecrab-cli-test-" + [guid]::NewGuid())

try {
    $env:FIRECRAB_CLI_ARCH = "x86_64"
    $asset = & $installer -PrintAsset
    if ($asset -ne "firecrab-cli-x86_64-windows.zip") {
        throw "unexpected Windows asset: $asset"
    }

    $env:FIRECRAB_CLI_ARCH = "ARM64"
    $armAsset = & $installer -PrintAsset
    if ($armAsset -ne "firecrab-cli-aarch64-windows.zip") {
        throw "unexpected Windows ARM64 asset: $armAsset"
    }
    $env:FIRECRAB_CLI_ARCH = "x86_64"

    $url = & $installer -Version "1.2.3" -PrintUrl
    $expected = "https://github.com/SteelCrab/firecrab/releases/download/v1.2.3/firecrab-cli-x86_64-windows.zip"
    if ($url -ne $expected) {
        throw "unexpected Windows URL: $url"
    }

    $check = & $installer -Check -InstallDir (Join-Path $scratch "bin")
    if ($check -notmatch "would install") {
        throw "check mode did not describe the install"
    }

    $release = Join-Path $scratch "releases\latest\download"
    $payload = Join-Path $scratch "payload"
    $installDir = Join-Path $scratch "bin"
    New-Item -ItemType Directory -Force -Path $release, $payload | Out-Null
    $fixtureBinary = Join-Path $payload "firecrab.exe"
    [System.IO.File]::WriteAllText($fixtureBinary, "firecrab fixture")
    $archive = Join-Path $release $asset
    Compress-Archive -Path $fixtureBinary -DestinationPath $archive -Force
    $sums = Join-Path $release "SHA256SUMS"
    $hash = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
    [System.IO.File]::WriteAllText($sums, "$hash  $asset`n")

    $env:FIRECRAB_RELEASE_BASE = ([System.Uri](Join-Path $scratch "releases")).AbsoluteUri.TrimEnd("/")
    $env:FIRECRAB_TEST_ALLOW_FILE_URL = "1"
    & $installer -InstallDir $installDir
    $installed = Join-Path $installDir "firecrab.exe"
    if (-not (Test-Path -LiteralPath $installed -PathType Leaf)) {
        throw "installer did not create firecrab.exe"
    }
    if ([System.IO.File]::ReadAllText($installed) -ne "firecrab fixture") {
        throw "installer changed the fixture binary"
    }
    [System.IO.File]::WriteAllText($fixtureBinary, "firecrab replacement")
    Compress-Archive -Path $fixtureBinary -DestinationPath $archive -Force
    $hash = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
    [System.IO.File]::WriteAllText($sums, "$hash  $asset`n")
    & $installer -InstallDir $installDir
    if ([System.IO.File]::ReadAllText($installed) -ne "firecrab replacement") {
        throw "installer did not atomically replace an existing binary"
    }
    $victim = Join-Path $scratch "victim.txt"
    [System.IO.File]::WriteAllText($victim, "keep victim")
    Remove-Item -LiteralPath $installed -Force
    New-Item -ItemType SymbolicLink -Path $installed -Target $victim | Out-Null
    & $installer -InstallDir $installDir
    if ([System.IO.File]::ReadAllText($victim) -ne "keep victim" -or
        ((Get-Item -LiteralPath $installed).Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "installer followed the destination symlink"
    }

    Push-Location $scratch
    try {
        & $installer -InstallDir "relative-bin"
    } finally {
        Pop-Location
    }
    $relativeBin = Join-Path $scratch "relative-bin"
    if (-not (Test-Path -LiteralPath (Join-Path $relativeBin "firecrab.exe"))) {
        throw "relative installation did not resolve against the PowerShell location"
    }
    # Linux PowerShell cannot persist the Windows user PATH registry value.
    # Keep that assertion on Windows; Docker checks the process value instead.
    $pathScope = if ($IsWindows) { "User" } else { "Process" }
    $userPathEntries = @([Environment]::GetEnvironmentVariable("Path", $pathScope) -split ";" | Where-Object { $_ })
    if ($userPathEntries -notcontains $relativeBin) {
        throw "relative installation did not persist an absolute PATH"
    }
    if ($userPathEntries -notcontains $installDir) {
        throw "installer did not add its directory to the user PATH"
    }
    [System.IO.File]::AppendAllText($archive, "tampered")
    $rejected = $false
    try {
        & $installer -InstallDir $installDir
    } catch {
        if ($_.Exception.Message -notmatch "checksum mismatch") { throw }
        $rejected = $true
    }
    if (-not $rejected -or [System.IO.File]::ReadAllText($installed) -ne "firecrab replacement") {
        throw "tampered release was not rejected with the installed binary preserved"
    }

    foreach ($unsafeBase in @("http://example.test/releases", "ftp://example.test/releases")) {
        $env:FIRECRAB_RELEASE_BASE = $unsafeBase
        try {
            & $installer -PrintUrl
            throw "unsafe release URL was accepted: $unsafeBase"
        } catch {
            if ($_.Exception.Message -match "unsafe release URL was accepted") { throw }
        }
    }
    $env:FIRECRAB_RELEASE_BASE = ([System.Uri](Join-Path $scratch "releases")).AbsoluteUri.TrimEnd("/")
    Remove-Item -LiteralPath Env:FIRECRAB_TEST_ALLOW_FILE_URL
    try {
        & $installer -PrintUrl
        throw "file release URL was accepted without explicit test opt-in"
    } catch {
        if ($_.Exception.Message -match "was accepted without") { throw }
    }
} finally {
    [Environment]::SetEnvironmentVariable("Path", $oldUserPath, "User")
    $env:Path = $oldProcessPath
    Restore-ProcessEnvironment -Name "FIRECRAB_CLI_ARCH" -Value $oldArch
    Restore-ProcessEnvironment -Name "FIRECRAB_RELEASE_BASE" -Value $oldReleaseBase
    Restore-ProcessEnvironment -Name "FIRECRAB_TEST_ALLOW_FILE_URL" -Value $oldAllowFileUrl
    Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Output "all tests passed"
