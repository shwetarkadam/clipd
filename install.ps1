# Install Clipd on Windows — downloads the latest release from GitHub.
#
# Usage (PowerShell):
#   irm https://raw.githubusercontent.com/shwetarkadam/clipd/main/install.ps1 | iex
#
# Installs to %LOCALAPPDATA%\clipd and adds it to user PATH.

$ErrorActionPreference = "Stop"

$Repo = "shwetarkadam/clipd"
$InstallDir = Join-Path $env:LOCALAPPDATA "clipd"

$Arch = if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq "X64") {
    "x86_64"
} elseif ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq "Arm64") {
    "arm64"
} else {
    Write-Error "Unsupported architecture"
    exit 1
}

if ($env:CLIPD_VERSION) {
    $Version = $env:CLIPD_VERSION
} else {
    Write-Host "Fetching latest release..."
    $Release = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
    $Version = $Release.tag_name
    if (-not $Version) {
        Write-Error "Could not determine latest version. Set CLIPD_VERSION and retry."
        exit 1
    }
}

$ZipName = "Clipd-windows-${Arch}-${Version}"
$Url = "https://github.com/$Repo/releases/download/$Version/${ZipName}.zip"

Write-Host ""
Write-Host "  Installing Clipd $Version (windows/$Arch)..."
Write-Host "  $Url"
Write-Host ""

$TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) "clipd-install-$(Get-Random)"
New-Item -ItemType Directory -Path $TmpDir | Out-Null

try {
    $ZipPath = Join-Path $TmpDir "clipd.zip"
    Invoke-WebRequest -Uri $Url -OutFile $ZipPath -UseBasicParsing

    Expand-Archive -Path $ZipPath -DestinationPath $TmpDir -Force

    $SrcDir = Join-Path $TmpDir $ZipName
    if (-not (Test-Path $SrcDir)) {
        Write-Error "Expected folder $ZipName inside zip"
        exit 1
    }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null

    foreach ($Bin in @("clipd.exe", "clipd-ui.exe")) {
        $Src = Join-Path $SrcDir $Bin
        if (Test-Path $Src) {
            Copy-Item -Path $Src -Destination (Join-Path $InstallDir $Bin) -Force
            Write-Host "  Copied $Bin -> $InstallDir"
        }
    }

    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($UserPath -notlike "*$InstallDir*") {
        [Environment]::SetEnvironmentVariable("Path", "$UserPath;$InstallDir", "User")
        Write-Host "  Added $InstallDir to user PATH"
    }
} finally {
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
}

Write-Host ""
Write-Host "  Done! Clipd installed."
Write-Host ""
Write-Host "  Next steps:"
Write-Host "    1. Open a new terminal so PATH takes effect"
Write-Host "    2. Start the daemon:  clipd daemon"
Write-Host "    3. Start the tray:    clipd-ui"
Write-Host ""
Write-Host "  CLI: clipd list | clipd search | clipd slots"
Write-Host ""
