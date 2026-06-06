# clipd — Browser Extension: Link Slot Copy

One job: hover any icon or element that wraps a link, press the slot-copy
chord, and the URL goes directly into a clipd slot — no clicking, no selecting,
no right-click menu.

## How it works

```
You hover over a LinkedIn icon  →  extension tracks <a href="https://…">
You press ⌘⇧+1 (Mac)           →  extension intercepts the chord
                                   POSTs { url, slot: 1 } to localhost:51234
                                   daemon stores it in slot 1
                                   toast: "📋 Copied to slot 1"
You press ⌘⌥+1 anywhere        →  daemon pastes slot 1 (normal clipd flow)
```

The extension talks directly to the clipd HTTP API — no clipboard relay,
no timing games, explicit confirmed write.

## Hotkeys

| Platform      | Copy chord      | Slot |
|---------------|-----------------|------|
| Mac           | ⌘ + Shift + 1–9 | 1–9  |
| Windows/Linux | Ctrl + Shift + 1–9 | 1–9 |

When the cursor is **not** over a link the chord is left alone — the clipd
daemon's OS-level handler fires as normal (text copy to slot).

## Architecture

```
[browser page]
  content_script.js
    - mouseover  → track currentHoveredElement
    - keydown ⌘⇧+N → find nearest <a href> in DOM
                    → sendMessage { PUSH_LINK, url, slot }
                    → show toast on response

[service worker]
  background.js
    - onMessage PUSH_LINK
    - POST http://localhost:51234/push  { "url": "…", "slot": N }
    - return { ok, slot } or { ok: false, error }

[clipd daemon — Rust]
  HTTP listener on 127.0.0.1:51234
    POST /push  → copy_to_slot(slot, url) + save_slots()
    GET  /health → { ok: true, version }
```

## Installation

### Prerequisites

clipd daemon **must be running** (`clipd start` or via Clipd.app).
Requires clipd v0.1.1+ (the version that ships the HTTP API).

### Chrome / Edge / Brave

1. `chrome://extensions` → enable **Developer mode**
2. **Load unpacked** → select this `clipd-browser-extension/` folder
3. Done — no popup, no config needed

### Firefox

1. `about:debugging#/runtime/this-firefox`
2. **Load Temporary Add-on…** → select `manifest.json`

## Error toasts

| Toast | Meaning |
|-------|---------|
| `clipd daemon not running. Start it with: clipd start` | Daemon is off |
| `Update clipd to enable browser support` | Daemon too old (no HTTP API) |
| `No link found under cursor` | Hovered element has no href |
| `Slot N is out of range (1–9)` | Internal guard hit |
