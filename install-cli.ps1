<#
.SYNOPSIS
Installs only the cross-platform firecrab client for the current Windows user.
#>
[CmdletBinding()]
param(
    [string]$Version = $(if ($env:FIRECRAB_VERSION) { $env:FIRECRAB_VERSION } else { "latest" }),
    [string]$InstallDir = $(
        if ($env:FIRECRAB_INSTALL_DIR) {
            $env:FIRECRAB_INSTALL_DIR
        } elseif ($env:LOCALAPPDATA) {
            Join-Path $env:LOCALAPPDATA "Programs\firecrab\bin"
        } else {
            Join-Path $HOME "firecrab\bin"
        }
    ),
    [switch]$Check,
    [switch]$PrintAsset,
    [switch]$PrintUrl
)

$ErrorActionPreference = "Stop"
$releaseBase = if ($env:FIRECRAB_RELEASE_BASE) {
    $env:FIRECRAB_RELEASE_BASE.TrimEnd("/")
} else {
    "https://github.com/SteelCrab/firecrab/releases"
}
$releaseUri = $null
if (-not [System.Uri]::TryCreate($releaseBase, [System.UriKind]::Absolute, [ref]$releaseUri)) {
    throw "FIRECRAB_RELEASE_BASE must be an absolute HTTPS URL"
}
if ($releaseUri.Scheme -eq "file") {
    if ($env:FIRECRAB_TEST_ALLOW_FILE_URL -ne "1") {
        throw "file:// release roots require FIRECRAB_TEST_ALLOW_FILE_URL=1"
    }
} elseif ($releaseUri.Scheme -ne "https") {
    throw "FIRECRAB_RELEASE_BASE must use HTTPS"
}

$rawArchitecture = if ($env:FIRECRAB_CLI_ARCH) {
    $env:FIRECRAB_CLI_ARCH
} else {
    [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
}
$architecture = switch -Regex ($rawArchitecture) {
    "^(X64|x86_64|amd64)$" { "x86_64"; break }
    default { throw "unsupported Windows client architecture: $rawArchitecture" }
}

$asset = "firecrab-cli-$architecture-windows.zip"
$normalizedVersion = if ([string]::IsNullOrWhiteSpace($Version) -or $Version -eq "latest") {
    "latest"
} elseif ($Version.StartsWith("v")) {
    $Version
} else {
    "v$Version"
}
$downloadRoot = if ($normalizedVersion -eq "latest") {
    "$releaseBase/latest/download"
} else {
    "$releaseBase/download/$normalizedVersion"
}
$assetUrl = "$downloadRoot/$asset"

if ($PrintAsset) {
    Write-Output $asset
    return
}
if ($PrintUrl) {
    Write-Output $assetUrl
    return
}
if ($Check) {
    Write-Output "would install $assetUrl to $(Join-Path $InstallDir 'firecrab.exe')"
    return
}

function Install-ClientBinary {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Source,
        [Parameter(Mandatory = $true)]
        [string]$DestinationDirectory
    )

    New-Item -ItemType Directory -Force -Path $DestinationDirectory | Out-Null
    $DestinationDirectory = (Resolve-Path -LiteralPath $DestinationDirectory).ProviderPath
    $destination = Join-Path $DestinationDirectory "firecrab.exe"
    $temporaryOutput = Join-Path $DestinationDirectory (
        ".firecrab-install-" + [guid]::NewGuid().ToString("N")
    )
    try {
        $sourceStream = [System.IO.File]::Open(
            $Source,
            [System.IO.FileMode]::Open,
            [System.IO.FileAccess]::Read,
            [System.IO.FileShare]::Read
        )
        try {
            $outputStream = [System.IO.File]::Open(
                $temporaryOutput,
                [System.IO.FileMode]::CreateNew,
                [System.IO.FileAccess]::Write,
                [System.IO.FileShare]::None
            )
            try {
                $sourceStream.CopyTo($outputStream)
            } finally {
                $outputStream.Dispose()
            }
        } finally {
            $sourceStream.Dispose()
        }

        $existing = Get-Item -LiteralPath $destination -Force -ErrorAction SilentlyContinue
        if ($null -ne $existing -and $existing.PSIsContainer) {
            throw "install destination $destination is a directory"
        }
        if ($null -ne $existing -and
            (($existing.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0)) {
            Remove-Item -LiteralPath $destination -Force
            $existing = $null
        }
        if ($null -ne $existing) {
            [System.IO.File]::Replace($temporaryOutput, $destination, [System.Management.Automation.Language.NullString]::Value)
        } else {
            [System.IO.File]::Move($temporaryOutput, $destination)
        }
    } finally {
        if (Test-Path -LiteralPath $temporaryOutput) {
            Remove-Item -LiteralPath $temporaryOutput -Force -ErrorAction SilentlyContinue
        }
    }
}

$temporary = Join-Path ([System.IO.Path]::GetTempPath()) ("firecrab-cli-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $temporary | Out-Null
try {
    $archive = Join-Path $temporary $asset
    $sums = Join-Path $temporary "SHA256SUMS"
    if ($releaseUri.Scheme -eq "file") {
        Copy-Item -LiteralPath ([System.Uri]$assetUrl).LocalPath -Destination $archive
        Copy-Item -LiteralPath ([System.Uri]"$downloadRoot/SHA256SUMS").LocalPath -Destination $sums
    } else {
        Invoke-WebRequest -UseBasicParsing -Uri $assetUrl -OutFile $archive
        Invoke-WebRequest -UseBasicParsing -Uri "$downloadRoot/SHA256SUMS" -OutFile $sums
    }

    $checksumLine = Get-Content $sums | Where-Object {
        $fields = $_ -split "\s+"
        if ($fields.Count -lt 2) { return $false }
        $listed = $fields[-1].TrimStart("*").Replace("\", "/").Split("/")[-1]
        $listed -eq $asset
    } | Select-Object -First 1
    if (-not $checksumLine) {
        throw "checksum missing for $asset"
    }
    $expected = ($checksumLine -split "\s+")[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "checksum mismatch for $asset"
    }

    $unpacked = Join-Path $temporary "unpacked"
    Expand-Archive -Path $archive -DestinationPath $unpacked
    $source = Join-Path $unpacked "firecrab.exe"
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "release archive does not contain firecrab.exe"
    }
    Install-ClientBinary -Source $source -DestinationDirectory $InstallDir
    $InstallDir = (Resolve-Path -LiteralPath $InstallDir).ProviderPath
} finally {
    Remove-Item -LiteralPath $temporary -Recurse -Force -ErrorAction SilentlyContinue
}

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
$pathEntries = @($userPath -split ";" | Where-Object { $_ })
if ($pathEntries -notcontains $InstallDir) {
    $updatedPath = (@($pathEntries) + $InstallDir) -join ";"
    [Environment]::SetEnvironmentVariable("Path", $updatedPath, "User")
    $env:Path = "$InstallDir;$env:Path"
    Write-Output "added $InstallDir to the user PATH; open a new terminal to use it"
}
Write-Output "installed firecrab to $(Join-Path $InstallDir 'firecrab.exe')"
