# Clipd for Windows

Clipd gives you clipboard history plus multiple independent clipboard slots.

## Start Clipd

Double-click `clipd-gui.exe` for the normal app window. It starts the background daemon automatically.

For logs while testing hotkeys, run `ClipdDebug.bat`.

## Multi-Slot Shortcuts

| Action | Keys |
| --- | --- |
| Copy to numeric slot N | `Ctrl+C` xN |
| Paste numeric slot N | `Ctrl+V` xN |
| Smart paste | `Ctrl+Shift+V` |
| Open Clipd window | `Ctrl+G` |

Direct alphabet-slot hotkeys are disabled on Windows because global `Win` / `Alt` / `Ctrl+Alt` letter chords collide with Windows, browsers, app shortcuts, and non-US keyboard layouts. Use the Clipd window/palette for letter aliases until the dedicated alphabet picker lands.

Examples:

- Slot 5: copy with `Ctrl+C` five times, paste with `Ctrl+V` five times.
- Normal paste is `Ctrl+V` once, which pastes slot 1.

## Install

Run PowerShell as your user from this folder:

```powershell
.\install.ps1
```

This installs Clipd under `%LOCALAPPDATA%\Clipd` and creates Start Menu shortcuts.

## Notes

Some Windows or app-level shortcuts may already be reserved. Clipd logs any hotkey registration conflicts in debug mode.
