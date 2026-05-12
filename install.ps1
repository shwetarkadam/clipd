# Install Clipd on Windows — downloads the latest release from GitHub.
#
# Usage (PowerShell — recommended):
#   irm https://raw.githubusercontent.com/shwetarkadam/clipd/main/install.ps1 | iex
#
# Same install, using curl (use curl.exe in PowerShell; plain "curl" is an alias):
#   curl.exe -fsSL https://raw.githubusercontent.com/shwetarkadam/clipd/main/install.ps1 -o "$env:TEMP\clipd-install.ps1"
#   powershell -NoProfile -ExecutionPolicy Bypass -File "$env:TEMP\clipd-install.ps1"
#
# From cmd.exe:
#   curl -fsSL https://raw.githubusercontent.com/shwetarkadam/clipd/main/install.ps1 -o "%TEMP%\clipd-install.ps1" && powershell -NoProfile -ExecutionPolicy Bypass -File "%TEMP%\clipd-install.ps1"
#
# This script:
#   1. Downloads the latest release from GitHub
#   2. Installs binaries to %LOCALAPPDATA%\Clipd
#   3. Adds to user PATH
#   4. Creates Start Menu shortcuts
#   5. Creates Desktop shortcut
#   6. Configures auto-start on login
#   7. Launches Clipd

$ErrorActionPreference = "Stop"

$Repo = "shwetarkadam/clipd"
$InstallDir = Join-Path $env:LOCALAPPDATA "Clipd"

function Write-Info($msg)  { Write-Host "  -> $msg" -ForegroundColor Cyan }
function Write-Ok($msg)    { Write-Host "  OK $msg" -ForegroundColor Green }
function Write-Warn($msg)  { Write-Host "  !! $msg" -ForegroundColor Yellow }

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
    Write-Info "Fetching latest release..."
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
Write-Host "  Clipd $Version (windows/$Arch)" -ForegroundColor White
Write-Host ""

$TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) "clipd-install-$(Get-Random)"
New-Item -ItemType Directory -Path $TmpDir | Out-Null

try {
    Write-Info "Downloading $Url..."
    $ZipPath = Join-Path $TmpDir "clipd.zip"
    Invoke-WebRequest -Uri $Url -OutFile $ZipPath -UseBasicParsing

    Expand-Archive -Path $ZipPath -DestinationPath $TmpDir -Force

    $SrcDir = Join-Path $TmpDir $ZipName
    if (-not (Test-Path $SrcDir)) {
        Write-Error "Expected folder $ZipName inside zip"
        exit 1
    }

    # ── Install binaries ──
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null

    foreach ($Bin in @("clipd.exe", "clipd-ui.exe", "clipd-gui.exe", "clipd-mcp.exe")) {
        $Src = Join-Path $SrcDir $Bin
        if (Test-Path $Src) {
            Copy-Item -Path $Src -Destination (Join-Path $InstallDir $Bin) -Force
            Write-Ok "Installed $Bin"
        }
    }

    # ── Add to PATH ──
    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($UserPath -notlike "*$InstallDir*") {
        [Environment]::SetEnvironmentVariable("Path", "$UserPath;$InstallDir", "User")
        $env:Path = "$env:Path;$InstallDir"
        Write-Ok "Added $InstallDir to user PATH"
    } else {
        Write-Ok "$InstallDir already in PATH"
    }

    # ── Start Menu shortcuts ──
    $StartMenu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
    $ClipdFolder = Join-Path $StartMenu "Clipd"
    New-Item -ItemType Directory -Path $ClipdFolder -Force | Out-Null

    $WshShell = New-Object -ComObject WScript.Shell

    $TrayLnk = Join-Path $ClipdFolder "Clipd.lnk"
    $Shortcut = $WshShell.CreateShortcut($TrayLnk)
    $Shortcut.TargetPath = Join-Path $InstallDir "clipd-ui.exe"
    $Shortcut.WorkingDirectory = $InstallDir
    $Shortcut.Description = "Clipd - Clipboard Manager (Tray)"
    $Shortcut.Save()

    $GuiLnk = Join-Path $ClipdFolder "Clipd GUI.lnk"
    $Shortcut = $WshShell.CreateShortcut($GuiLnk)
    $Shortcut.TargetPath = Join-Path $InstallDir "clipd-gui.exe"
    $Shortcut.WorkingDirectory = $InstallDir
    $Shortcut.Description = "Clipd - GUI Search"
    $Shortcut.Save()

    $CLILnk = Join-Path $ClipdFolder "Clipd CLI.lnk"
    $Shortcut = $WshShell.CreateShortcut($CLILnk)
    $Shortcut.TargetPath = Join-Path $InstallDir "clipd.exe"
    $Shortcut.WorkingDirectory = $InstallDir
    $Shortcut.Description = "Clipd - Command Line"
    $Shortcut.Save()

    Write-Ok "Created Start Menu shortcuts"

    # ── Desktop shortcut ──
    $DesktopLnk = Join-Path $env:USERPROFILE "Desktop\Clipd.lnk"
    $Shortcut = $WshShell.CreateShortcut($DesktopLnk)
    $Shortcut.TargetPath = Join-Path $InstallDir "clipd-ui.exe"
    $Shortcut.WorkingDirectory = $InstallDir
    $Shortcut.Description = "Clipd - Clipboard Manager"
    $Shortcut.Save()
    Write-Ok "Created Desktop shortcut"

    # ── Auto-start (Startup folder shortcut) ──
    $StartupFolder = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Startup"
    $StartupLnk = Join-Path $StartupFolder "Clipd.lnk"
    $Shortcut = $WshShell.CreateShortcut($StartupLnk)
    $Shortcut.TargetPath = Join-Path $InstallDir "clipd-ui.exe"
    $Shortcut.WorkingDirectory = $InstallDir
    $Shortcut.Description = "Clipd - Auto-start"
    $Shortcut.Save()
    Write-Ok "Auto-start configured (Startup folder)"

    # ── Launch Clipd now ──
    Write-Info "Launching Clipd..."
    $TrayExe = Join-Path $InstallDir "clipd-ui.exe"
    if (Test-Path $TrayExe) {
        Start-Process -FilePath $TrayExe -WindowStyle Hidden
        Write-Ok "Clipd started (tray icon)"
    }

} finally {
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
}

Write-Host ""
Write-Ok "Done! Clipd $Version installed."
Write-Host ""
Write-Host "  Tray icon: check your system tray (bottom-right)"
Write-Host "  CLI:       clipd list | clipd search | clipd slots"
Write-Host "  GUI:       double-click the Desktop shortcut or Start Menu"
Write-Host ""
Write-Host "  Uninstall:  Remove-Item -Recurse `"`$env:LOCALAPPDATA\Clipd`""
Write-Host "             Then remove from PATH in System Settings > Environment Variables"
Write-Host ""
