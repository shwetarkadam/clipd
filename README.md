# clipd

> A clipboard manager for macOS, built in Rust.

clipd gives you **multiple independent clipboard slots** — so you can copy several things at once and paste any of them on demand, without losing what you copied before.

---

## The Problem with Your Clipboard Today

Your Mac has one clipboard. Every time you press `Cmd+C`, whatever you copied before is gone.

This is fine for simple tasks. But the moment you're doing anything real — filling a spreadsheet, moving code around, reorganising data — you're constantly switching windows, re-copying things you already had, and losing your flow.

clipd fixes this.

---

## Install

### Option 1 — One-line install (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/shwetarkadam/clipd/main/install.sh | bash
```

This downloads the latest release, copies **Clipd.app** to `/Applications`, and puts the `clipd` CLI in `/usr/local/bin`.

### Option 2 — Manual download

1. Go to [**Releases**](https://github.com/shwetarkadam/clipd/releases)
2. Download **`Clipd-macos-arm64-vX.X.X.zip`** (Apple Silicon) or **`Clipd-macos-x86_64-vX.X.X.zip`** (Intel)
3. Unzip, drag **Clipd.app** into **Applications**
4. Double-click **Clipd** to launch

### Option 3 — Build from source

Requires [Rust](https://rustup.rs/) (latest stable) and Xcode Command Line Tools.

```bash
git clone https://github.com/shwetarkadam/clipd.git
cd clipd
./release.sh
```

Then drag `target/release/Clipd.app` to Applications.

---

## First Launch — Permissions

macOS requires two permissions for clipd to work. You'll be prompted automatically on first launch:

| Permission | Why | Where to grant |
|-----------|-----|----------------|
| **Accessibility** | Simulating paste (Cmd+V) into apps | System Settings → Privacy & Security → Accessibility |
| **Input Monitoring** | Detecting Cmd+C / Ctrl+C hotkey taps | System Settings → Privacy & Security → Input Monitoring |

Grant both, then **restart Clipd** (Quit from menu bar → reopen).

> **Gatekeeper warning?** If macOS says "Clipd can't be opened because it is from an unidentified developer", go to **System Settings → Privacy & Security** and click **Open Anyway**.

---

## How It Works

When you open Clipd:
- A **menu bar icon** appears (top-right of your screen)
- The **daemon** starts automatically in the background
- The **main window** opens for visual slot access

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

clipd supports two hotkey styles — pick whichever feels natural:

**Excel / developer mode:**

Enable this in **Paste Settings → Slot Input Mode**.

| Action | Hotkey |
|--------|--------|
| Copy to slots 1–9 | `Cmd+C` × N |
| Paste slot 1 | `Cmd+V` |
| Paste from slots 2–9 | `Cmd+V` × N |
| Copy to slots 11–30 | `Option+C` × N |
| Paste from slots 11–30 | `Option+V` × N |
| Paste next queued slot | `Cmd+Option+V` |

The HUD shows the active slot bank while tapping, so you can stop when the target slot is highlighted.

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

**Other shortcuts:**

| Shortcut | Action |
|----------|--------|
| `Ctrl+R` | Open search TUI |
| `Ctrl+G` (macOS/Linux), `Alt+G` (Windows) | Open GUI window |
| `Ctrl+Shift+V` | Smart paste (transform clipboard before pasting) |
| `Cmd+Option+V` | Sequence paste (auto-increment through slots) |

Action fires **0.35s** after the last tap.

---

## Menu Bar

Click the clipd icon in the menu bar for quick access:

- **Start / Stop daemon**
- **Open clipd search** (GUI or TUI depending on mode)
- **HUD slot overlay** — enable/disable the floating overlay that shows slot numbers as you tap
- **Developer mode (TUI)** — switch search to terminal instead of GUI
- **Quit clipd UI**

---

## Features

- **Multi-slot clipboard** — copy multiple things, paste any of them independently
- **Clipboard history** — full searchable history of everything you've copied
- **Hotkeys** — multi-tap `Cmd+C` / `Cmd+V` feels completely natural
- **Interactive search** — `Ctrl+R` to fuzzy search your history
- **Native macOS UI** — quick visual access to your slots and settings
- **HUD overlay** — floating display shows which slot you're targeting as you tap
- **Smart paste** — transform clipboard content before pasting (trim, format JSON, fix grammar, etc.)
- **Lightweight daemon** — runs quietly in the background, zero config

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
├── clipd-ui        # Menu bar tray app (launches daemon + GUI)
├── clipd-gui       # Native macOS GUI (eframe)
├── clipd-hud       # Swift HUD overlay for slot tap feedback
├── clipd-mcp       # MCP server integration
└── packaging/      # macOS app bundle scripts
```

---

## AI assistants (MCP)

clipd ships an MCP server (`clipd-mcp`) so AI assistants can search your
clipboard history — including by meaning, and inside screenshots via OCR text —
put results on your clipboard, and manage snippets. Everything runs locally
against clipd's own database.

**Claude Code:**

```bash
claude mcp add clipd -- /Applications/Clipd.app/Contents/MacOS/clipd-mcp
```

**Claude Desktop** (`claude_desktop_config.json`) / **Cursor** (`.cursor/mcp.json`):

```json
{
  "mcpServers": {
    "clipd": { "command": "/Applications/Clipd.app/Contents/MacOS/clipd-mcp" }
  }
}
```

On Windows the binary is `%LOCALAPPDATA%\Clipd\clipd-mcp.exe`; on Linux, wherever
you installed `clipd-mcp`.

Tools: `search_clips` (hybrid keyword + semantic; embeddings when configured,
local TF-IDF otherwise), `get_recent`, `get_clip`, `set_clipboard`, `add_clip`,
`list_slots`, `list_snippets`, `save_snippet`, `list_collections`,
`get_collection`, `transform`, `list_transforms`, `get_sessions`, `stats`.

Try: "find that API key format I copied yesterday", "put the summary of my
last 5 clips on my clipboard", "save my address as a snippet triggered by 'addr'".

---

## Uninstall

```bash
# Remove the app
rm -rf /Applications/Clipd.app

# Remove CLI (if installed)
sudo rm -f /usr/local/bin/clipd

# Remove data (clipboard history, settings)
rm -rf ~/Library/Application\ Support/clipd
rm -rf ~/Library/Logs/clipd-ui-daemon.log
```

---

## License

Licensed under the [Business Source License 1.1](./LICENSE).
Free for personal use. Commercial use requires a license from the author.

---

## Author

Made by [Shweta Kadam](https://github.com/shwetarkadam)
