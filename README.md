# clipd

> A clipboard manager for macOS, Windows, and Linux — built in Rust.

clipd gives you **multiple independent clipboard slots** — so you can copy several things at once and paste any of them on demand, without losing what you copied before.

---

## The Problem with Your Clipboard Today

Your OS has one clipboard. Every time you press `Cmd+C` (or `Ctrl+C`), whatever you copied before is gone.

This is fine for simple tasks. But the moment you're doing anything real — filling a spreadsheet, moving code around, reorganising data — you're constantly switching windows, re-copying things you already had, and losing your flow.

clipd fixes this.

---

## Install

### macOS

**One-line install (recommended):**

```bash
curl -fsSL https://raw.githubusercontent.com/shwetarkadam/clipd/main/install.sh | bash
```

This downloads the latest release, copies **Clipd.app** to `/Applications`, and puts the `clipd` CLI in `/usr/local/bin`.

**Manual download:**

1. Go to [**Releases**](https://github.com/shwetarkadam/clipd/releases)
2. Download **`Clipd-macos-arm64-vX.X.X.zip`** (Apple Silicon) or **`Clipd-macos-x86_64-vX.X.X.zip`** (Intel)
3. Unzip, drag **Clipd.app** into **Applications**
4. Double-click **Clipd** to launch

### Linux

**One-line install:**

```bash
curl -fsSL https://raw.githubusercontent.com/shwetarkadam/clipd/main/install.sh | bash
```

Installs `clipd` and `clipd-ui` to `~/.local/bin`. Make sure that's in your `PATH`.

### Windows

**PowerShell (recommended):**

```powershell
irm https://raw.githubusercontent.com/shwetarkadam/clipd/main/install.ps1 | iex
```

Installs to `%LOCALAPPDATA%\clipd` and adds it to your user PATH.

**Manual download:**

1. Go to [**Releases**](https://github.com/shwetarkadam/clipd/releases)
2. Download **`Clipd-windows-x86_64-vX.X.X.zip`**
3. Extract and add the folder to your PATH

### Build from source

Requires [Rust](https://rustup.rs/) (latest stable). On macOS, also requires Xcode Command Line Tools.

```bash
git clone https://github.com/shwetarkadam/clipd.git
cd clipd
cargo build --release
```

Binaries land in `target/release/`. On macOS you can also run `./release.sh` to get a bundled `Clipd.app`.

---

## First Launch

### macOS — Permissions

macOS requires two permissions for clipd to work. You'll be prompted on first launch:

| Permission | Why | Where to grant |
|-----------|-----|----------------|
| **Accessibility** | Simulating paste (Cmd+V) into apps | System Settings → Privacy & Security → Accessibility |
| **Input Monitoring** | Detecting Cmd+C / Ctrl+C hotkey taps | System Settings → Privacy & Security → Input Monitoring |

Grant both, then **restart Clipd** (Quit from menu bar → reopen).

> **Gatekeeper warning?** If macOS says "Clipd can't be opened because it is from an unidentified developer", go to **System Settings → Privacy & Security** and click **Open Anyway**.

### Windows / Linux

No special permissions needed. Just run `clipd daemon` to start the background service, and `clipd-ui` for the tray icon.

---

## How It Works

When you open Clipd:
- A **tray icon** appears (menu bar on macOS, system tray on Windows/Linux)
- The **daemon** starts automatically in the background
- Your clipboard is monitored and slots are ready

That's it. No config files, no setup.

---

## Multi-Slot Clipboard

Instead of one clipboard, clipd gives you **up to 30 slots**. Think of them as clipboard 1, clipboard 2, clipboard 3 — all active at the same time.

```
Normal clipboard:         clipd:
┌─────────┐               ┌─────────┐  ┌─────────┐  ┌─────────┐
│  item A │  ← Cmd+C      │ slot 1  │  │ slot 2  │  │ slot 3  │
│         │               │ item A  │  │ item B  │  │ item C  │
│ (item B │               └─────────┘  └─────────┘  └─────────┘
│  is now │                  ↑              ↑              ↑
│   gone) │               Cmd+V×2       Cmd+V×3       Cmd+V×4
└─────────┘
```

---

## Hotkeys

### macOS (multi-tap via rdev)

clipd supports two hotkey styles — pick whichever feels natural:

**Option A — Cmd multi-tap:**

| Action | Hotkey |
|--------|--------|
| Copy to slot 1 | `Cmd+C` × 2 |
| Copy to slot 2 | `Cmd+C` × 3 |
| Paste slot 1 | `Cmd+V` × 2 |
| Paste slot 2 | `Cmd+V` × 3 |

**Option B — Ctrl tap (after Cmd+C):**

| Action | Hotkey |
|--------|--------|
| Copy to slot 1 | `Ctrl+C` × 1 |
| Copy to slot 2 | `Ctrl+C` × 2 |
| Paste slot 1 | `Ctrl+V` × 1 |
| Paste slot 2 | `Ctrl+V` × 2 |

### Windows / Linux (global-hotkey)

| Action | Hotkey |
|--------|--------|
| Copy to slot N | `Ctrl+Super+N` (N = 1–9) |
| Paste slot N | `Ctrl+Super+Alt+N` |
| Smart paste | `Ctrl+Shift+V` |
| Open TUI search | `Ctrl+R` or `Ctrl+T` |
| Open GUI | `Ctrl+G` |

### Common shortcuts (all platforms)

| Shortcut | Action |
|----------|--------|
| `Ctrl+Shift+V` | Smart paste (transform clipboard before pasting) |

Action fires **0.35s** after the last tap (macOS multi-tap only).

---

## Menu Bar / Tray

Click the clipd icon for quick access:

- **Start / Stop daemon**
- **Open clipd search** (GUI or TUI depending on mode)
- **HUD slot overlay** — enable/disable the floating overlay that shows slot numbers as you tap (macOS: native HUD, Windows/Linux: desktop notification)
- **Developer mode (TUI)** — switch search to terminal instead of GUI
- **Quit clipd UI**

---

## Features

- **Multi-slot clipboard** — copy multiple things, paste any of them independently
- **Clipboard history** — full searchable history of everything you've copied
- **Hotkeys** — multi-tap Cmd+C / Cmd+V on macOS; global hotkeys on Windows/Linux
- **Interactive search** — fuzzy search your clipboard history
- **Tray app** — quick visual access to your slots and settings
- **HUD overlay** — floating display shows which slot you're targeting (macOS: native Swift overlay, Windows/Linux: desktop notification)
- **Smart paste** — transform clipboard content before pasting (trim, format JSON, fix grammar, etc.)
- **Self-update** — `clipd update` checks for new versions
- **Lightweight daemon** — runs quietly in the background, zero config

---

## Platform Support

| Feature | macOS | Windows | Linux |
|---------|-------|---------|-------|
| Multi-slot clipboard | Yes | Yes | Yes |
| Clipboard history + search | Yes | Yes | Yes |
| Tray app | Yes | Yes | Yes |
| HUD notifications | Native overlay | Desktop notification | Desktop notification |
| Multi-tap hotkeys (Cmd×N) | Yes (rdev) | — | — |
| Global hotkeys (Ctrl+Super+N) | — | Yes | Yes |
| Smart paste | Yes | Yes | Yes |
| Paste simulation | AppleScript | Enigo | Enigo |
| Frontmost app detection | Yes | — | — |
| Self-update | Yes | Yes | Yes |

---

## CLI Usage

The `clipd` binary doubles as a full CLI:

```bash
clipd              # Launch GUI + daemon (default)
clipd daemon       # Start daemon only (headless)
clipd list         # Show recent clips
clipd search       # Interactive search (TUI)
clipd search <q>   # Text search
clipd paste <slot> # Output slot to stdout
clipd slots        # Show slot contents
clipd stats        # Usage statistics
clipd clear        # Clear history/slots
clipd update       # Check for updates
```

---

## Who Is This For?

### Spreadsheet Users
Copy values from multiple cells, switch to your destination, paste them all — no switching back and forth.

### Developers
Keep a function signature in slot 1 while you copy its body to slot 2. Hold API keys, git hashes, and error messages in parallel slots.

### Everyone
Ever filled a long form and lost something because you needed to copy something else? clipd keeps everything available.

---

## Project Structure

```
clipd/
├── clipd-core      # Shared core logic (clipboard, storage, transforms)
├── clipd-daemon    # Background service (hotkeys, clipboard watcher)
├── clipd-cli       # Command-line interface
├── clipd-tui       # Terminal UI for search
├── clipd-ui        # Tray app (launches daemon + GUI)
├── clipd-gui       # GUI (eframe)
├── clipd-hud       # Swift HUD overlay (macOS only)
├── clipd-mcp       # MCP server integration
└── packaging/      # App bundle scripts
```

---

## Uninstall

### macOS

```bash
rm -rf /Applications/Clipd.app
sudo rm -f /usr/local/bin/clipd
rm -rf ~/Library/Application\ Support/clipd
```

### Linux

```bash
rm -f ~/.local/bin/clipd ~/.local/bin/clipd-ui
rm -rf ~/.local/share/clipd
```

### Windows (PowerShell)

```powershell
Remove-Item -Recurse "$env:LOCALAPPDATA\clipd"
# Then remove clipd from your PATH in System Settings > Environment Variables
```

---

## License

Licensed under the [Business Source License 1.1](./LICENSE).
Free for personal use. Commercial use requires a license from the author.

---

## Author

Made by [Shweta Kadam](https://github.com/shwetarkadam)
