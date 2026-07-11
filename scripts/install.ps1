param(
    [string]$Version = "latest",
    [string]$Repository = "Garfreak-07/Coder",
    [string]$InstallDir = "",
    [string]$TempDir = "",
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"

function Resolve-Target {
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
    if ($IsWindows -or $env:OS -eq "Windows_NT") {
        if ($arch -ne "x64") {
            throw "Unsupported Windows architecture: $arch"
        }
        return @{
            Target = "x86_64-pc-windows-msvc"
            Archive = "coder-rust-x86_64-pc-windows-msvc.zip"
            Binary = "coder-rust.exe"
        }
    }
    if ($IsMacOS) {
        if ($arch -eq "arm64") {
            return @{
                Target = "aarch64-apple-darwin"
                Archive = "coder-rust-aarch64-apple-darwin.tar.gz"
                Binary = "coder-rust"
            }
        }
        if ($arch -eq "x64") {
            return @{
                Target = "x86_64-apple-darwin"
                Archive = "coder-rust-x86_64-apple-darwin.tar.gz"
                Binary = "coder-rust"
            }
        }
        throw "Unsupported macOS architecture: $arch"
    }
    if ($IsLinux) {
        if ($arch -ne "x64") {
            throw "Unsupported Linux architecture: $arch"
        }
        return @{
            Target = "x86_64-unknown-linux-gnu"
            Archive = "coder-rust-x86_64-unknown-linux-gnu.tar.gz"
            Binary = "coder-rust"
        }
    }
    throw "Unsupported operating system."
}

function Resolve-InstallDir {
    param([string]$Requested)
    if ($Requested) {
        return $Requested
    }
    if ($IsWindows -or $env:OS -eq "Windows_NT") {
        return Join-Path $env:LOCALAPPDATA "Coder\bin"
    }
    return Join-Path $HOME ".local/bin"
}

function Resolve-InstallerTempDir {
    param([string]$Requested)
    if ($Requested) {
        return $Requested
    }
    if ($env:CODER_INSTALL_TMPDIR) {
        return $env:CODER_INSTALL_TMPDIR
    }
    return [System.IO.Path]::GetTempPath()
}

$target = Resolve-Target
$installRoot = Resolve-InstallDir $InstallDir
$tempBase = Resolve-InstallerTempDir $TempDir
$releaseBase = if ($Version -eq "latest") {
    "https://github.com/$Repository/releases/latest/download"
} else {
    "https://github.com/$Repository/releases/download/$Version"
}
$assetUrl = "$releaseBase/$($target.Archive)"

Write-Host "coder-rust installer"
Write-Host "Target: $($target.Target)"
Write-Host "Archive: $($target.Archive)"
Write-Host "InstallDir: $installRoot"
Write-Host "TempDir: $tempBase"
Write-Host "URL: $assetUrl"

if ($DryRun) {
    Write-Host "DryRun: no download or install performed."
    exit 0
}

New-Item -ItemType Directory -Force -Path $tempBase | Out-Null
$tempRoot = Join-Path $tempBase ("coder-rust-install-" + [System.Guid]::NewGuid().ToString("N"))
$archivePath = Join-Path $tempRoot $target.Archive
$extractDir = Join-Path $tempRoot "extract"
New-Item -ItemType Directory -Force -Path $tempRoot, $extractDir, $installRoot | Out-Null

try {
    Invoke-WebRequest -Uri $assetUrl -OutFile $archivePath
    if ($target.Archive.EndsWith(".zip")) {
        Expand-Archive -Path $archivePath -DestinationPath $extractDir -Force
    } else {
        tar -xzf $archivePath -C $extractDir
    }
    $binaryPath = Get-ChildItem -Path $extractDir -Recurse -File -Filter $target.Binary | Select-Object -First 1
    if (-not $binaryPath) {
        throw "Archive did not contain $($target.Binary)."
    }
    Copy-Item -LiteralPath $binaryPath.FullName -Destination (Join-Path $installRoot $target.Binary) -Force
    Write-Host "Installed $($target.Binary) to $installRoot"
    Write-Host "Next: coder-rust doctor"
} finally {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
}
