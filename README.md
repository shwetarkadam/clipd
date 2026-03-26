# clipd

> ⚠️ Early prototype — expect rough edges.

A clipboard manager for macOS, built in Rust. clipd gives you **multiple independent clipboard slots** — so you can copy several things at once and paste any of them on demand, without losing what you copied before.

---

## The Problem with Your Clipboard Today

Your Mac has one clipboard. Every time you press `Cmd+C`, whatever you copied before is gone.

This is fine for simple tasks. But the moment you're doing anything real — filling a spreadsheet, moving code around, reorganising data — you're constantly switching windows, re-copying things you already had, and losing your flow.

clipd fixes this.

---
🎬 See how clipd lets you copy multiple things and paste any of them instantly:


---

## Multi-Slot Clipboard

Instead of one clipboard, clipd gives you **multiple slots**. Think of them as clipboard 1, clipboard 2, clipboard 3 — all active at the same time.

Double-tap `Cmd+C` to copy to slot 1. Triple-tap to copy to slot 2. Then paste any slot back whenever you need it.

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

## Who Is This For?

### 📊 Excel / Spreadsheet Users

Copying values across sheets is painful. You grab a number from one cell, switch sheets, paste it, go back, grab the next one — and if you accidentally copy something else in between, you have to start over.

With clipd:
- Copy your first value → slot 1 (`Cmd+C` twice)
- Copy your second value → slot 2 (`Cmd+C` three times)
- Switch to your destination sheet
- Paste slot 1 where you need it (`Cmd+V` twice)
- Paste slot 2 where you need it (`Cmd+V` three times)

No switching back. No re-copying. No mistakes.

---

### 👩‍💻 Developers

How many times have you copied a variable name, then copied a file path, and lost the variable name? Or been refactoring and needed to juggle three snippets at once?

With clipd:
- Keep a function signature in slot 1 while you copy its body to slot 2
- Hold a git commit hash in slot 1 while searching for a related error message
- Copy an API key to slot 1 and an endpoint to slot 2 — paste both into your config without switching tabs
- Search your full clipboard history with `Ctrl+R` when you need something from earlier

---

### 🙋 General Mac Users

Ever filled a long form and lost a piece of info because you needed to copy something else mid-way? clipd keeps everything you copied available until you're done.

---

## Features

- 🗂️ **Multi-slot clipboard** — copy multiple things, paste any of them independently
- 📋 **Clipboard history** — full searchable history of everything you've copied
- ⌨️ **Hotkeys** — multi-tap `Cmd+C` / `Cmd+V` feels completely natural, no new shortcuts to memorise
- 🔍 **Interactive search** — `Ctrl+R` to fuzzy search your history in the terminal
- 🖥️ **Native macOS UI** — quick visual access to your slots
- ⚡ **Lightweight daemon** — runs quietly in the background, zero config

---

## Hotkeys

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

- `Ctrl+R` — open search TUI
- Action fires **0.35s** after the last tap

---

## CLI Usage

```bash
clipd daemon         # Start the clipboard daemon
clipd list           # Show recent clips
clipd search         # Interactive search (TUI)
clipd search <query> # Text search
clipd paste <slot>   # Output slot to stdout
clipd slots          # Show slot contents
clipd stats          # Usage statistics
clipd clear          # Clear history/slots
```

---

## Download

Pre-built binaries for macOS are available on the [Releases](https://github.com/shwetarkadam/clipd/releases) page.

```bash
chmod +x clipd
./clipd
```

> If macOS blocks the app, go to **System Settings → Privacy & Security** and click **Open Anyway**.

---

## Build from Source

Requires [Rust](https://rustup.rs/) (latest stable).

```bash
git clone https://github.com/shwetarkadam/clipd.git
cd clipd
cargo build --release
./target/release/clipd daemon
```

---

## Project Structure

```
clipd/
├── clipd-core      # Shared core logic
├── clipd-daemon    # Background service that watches the clipboard
├── clipd-cli       # Command-line interface
├── clipd-tui       # Terminal UI
└── clipd-ui        # Native macOS UI
```

---

## License

Licensed under the [Business Source License 1.1](./LICENSE).  
Free for personal use. Commercial use requires a license from the author.

---

## Author

Made by [Shweta Kadam](https://github.com/shwetarkadam)
