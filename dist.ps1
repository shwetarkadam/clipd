# Build clipd and package for Windows.
#
# Usage (PowerShell):
#   .\dist.ps1              # build + package (version from Cargo.toml)
#   .\dist.ps1 v0.2.0       # override version tag
#
# Produces:
#   target\release\Clipd-windows-x86_64-<ver>.zip
#     - clipd.exe, clipd-ui.exe, clipd-gui.exe, clipd-mcp.exe
#     - Clipd.bat, ClipdTray.vbs, ClipdDebug.bat
#     - install.ps1, README.md (same layout as GitHub Release zip)

$ErrorActionPreference = "Stop"

$Root = $PSScriptRoot
if (-not $Root) { $Root = "." }
Set-Location $Root

$Arch = "x86_64"
$Version = if ($args.Count -gt 0) { $args[0] } else {
    $line = Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]*)"' | Select-Object -First 1
    "v" + $line.Matches[0].Groups[1].Value
}

$PkgName = "Clipd-windows-${Arch}-${Version}"

Write-Host ""
Write-Host "  clipd dist - ${Version}  (windows/${Arch})"
Write-Host ""

# ── 1. Rust release build ──
Write-Host "==> cargo build --release"
cargo build --release
if ($LASTEXITCODE -ne 0) { Write-Error "Build failed"; exit 1 }

# ── 2. Package ──
Write-Host "==> Creating ${PkgName}.zip"

$Stage = "target\release\$PkgName"
if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
New-Item -ItemType Directory -Path $Stage -Force | Out-Null

foreach ($bin in @("clipd.exe", "clipd-ui.exe", "clipd-gui.exe", "clipd-mcp.exe", "clipd-overlay.exe")) {
    $src = "target\release\$bin"
    if (Test-Path $src) {
        Copy-Item $src $Stage
        Write-Host "    bundled $bin"
    }
}

# Double-click launcher (batch file)
Copy-Item "packaging\windows\Clipd.bat" $Stage -ErrorAction SilentlyContinue

# Silent launcher (no console window)
Copy-Item "packaging\windows\ClipdTray.vbs" $Stage -ErrorAction SilentlyContinue

Copy-Item "packaging\windows\ClipdDebug.bat" $Stage -ErrorAction SilentlyContinue

Copy-Item install.ps1 $Stage -ErrorAction SilentlyContinue
Copy-Item README.md $Stage -ErrorAction SilentlyContinue

$ZipPath = "target\release\$PkgName.zip"
if (Test-Path $ZipPath) { Remove-Item -Force $ZipPath }
Compress-Archive -Path $Stage -DestinationPath $ZipPath
Remove-Item -Recurse -Force $Stage

Write-Host ""
Write-Host "  Done!  -> $ZipPath"
Write-Host ""
Write-Host "  To install: unzip and double-click Clipd.bat (or ClipdTray.vbs for no console)"
Write-Host "  Or run:  .\install.ps1"
