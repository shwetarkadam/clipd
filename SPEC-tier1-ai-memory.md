# Spec — Tier 1: Local AI Memory (palette + named/source-aware recall)

Status: proposed · Scope: local-first, no API key required · Author: design thread

This spec turns clipd from "56 numbered slots" into "ask for what you copied."
It covers only the agreed Tier 1 — the parts that are **local, exact-first, and
need no cloud AI**. Smart transforms (clean table, JSON→type) are explicitly out
of scope here; they bolt onto step 2 of the palette later.

It is written against the current code. Key symbols referenced:
- `clipd-core/src/models.rs` — `ClipEntry`, `ContentType`, `SearchFilters`
- `clipd-core/src/store.rs` — `ClipStore`, schema/migrations, `search()`
- `clipd-core/src/semantic.rs` — `TfIdfIndex` (local TF-IDF search)
- `clipd-core/src/embedding.rs` — vector search (cloud; **stays optional**)
- `clipd-core/src/privacy.rs` — `PrivacyConfig`, `should_skip_clip`, `detect_sensitive`
- `clipd-daemon/src/daemon.rs` — `get_frontmost_app_name`, `open_slot_picker`,
  `HotkeyTick::SlotMemoryHud`, `save_text_to_slot`

---

## 0. The one correction that shapes everything

"Local semantic recall" must **not** use `embedding.rs`. `generate_embedding()`
calls an OpenAI-compatible `/embeddings` endpoint and hard-requires an API key
(`embedding.rs:13-18`). That's cloud. Tier 1's promise is *no key, nothing
leaves the machine*, so local recall is built from the two mechanisms that are
already 100% local:

1. **FTS5 full-text** (`ClipStore::search`, `store.rs:199`) — exact / keyword.
2. **TF-IDF cosine** (`TfIdfIndex`, `semantic.rs`) — meaning-ish, in-memory.

Cloud embeddings remain an optional *re-ranker* you can layer on later when a
key is present (`is_embedding_available`, `embedding.rs:211`). Tier 1 ships
without it.

---

## 1. Data model — clips remember what they are and where they came from

### 1.1 `ClipEntry` (`models.rs:139`)

Add source context and an optional user name. Keep everything backward-compatible.

```rust
pub struct ClipEntry {
    pub id: i64,
    pub content: String,
    pub content_type: ContentType,
    pub content_hash: String,
    pub source_app: Option<String>,    // exists
    pub timestamp: DateTime<Utc>,
    pub preview: String,
    pub slot: Option<u8>,
    // NEW — all optional, all local:
    pub source_title: Option<String>,  // window title, e.g. "auth.ts — myrepo"
    pub source_url: Option<String>,    // browser URL when app is a browser
    pub name: Option<String>,          // user-given name ("api", "bug"); NEVER AI-assigned
}
```

`ClipEntry::new` gains a `SourceContext` param instead of bare `source_app`:

```rust
pub struct SourceContext {
    pub app: Option<String>,
    pub title: Option<String>,
    pub url: Option<String>,
}
impl ClipEntry {
    pub fn new(content: String, src: SourceContext, slot: Option<u8>) -> Self { … }
    pub fn label(&self) -> String { /* name, else AI/heuristic label, else type icon */ }
}
```

`name` is **user-controlled and trusted**; AI may *suggest* a label but it lives
in a separate display path (`label()`), never overwrites `name`, and never
becomes an address. This is the exact-first boundary in code form.

### 1.2 Schema (`store.rs:run_migrations`, ~line 49)

Add three nullable columns via the same `ALTER TABLE … ADD COLUMN` pattern
already used for `slot` (`store.rs:72-80`), which is safe on existing DBs:

```sql
ALTER TABLE clips ADD COLUMN source_title TEXT;
ALTER TABLE clips ADD COLUMN source_url   TEXT;
ALTER TABLE clips ADD COLUMN name         TEXT;
CREATE INDEX IF NOT EXISTS idx_clips_name ON clips(name);
```

Extend the FTS5 virtual table (`store.rs:100`) to index `source_title` and
`source_url` so "stripe" / "auth.ts" recall works through the same query path:

```sql
CREATE VIRTUAL TABLE clips_fts USING fts5(
    content, preview, source_title, source_url,
    content='clips', content_rowid='id'
);
```

(Triggers `clips_ai/ad/au` at `store.rs:108-123` must be updated to carry the
two new columns. On an existing DB the FTS table needs a one-time rebuild —
gate it behind a `schema_version` row.)

### 1.3 Capture source at copy time (the cheap, high-value win)

Right now source is mostly lost: `save_text_to_slot` inserts
`ClipEntry::new(text, None, …)` (`daemon.rs:761`) — `source_app: None`. The
daemon already knows how to read the frontmost app (`get_frontmost_app_name`,
`daemon.rs:1255`; `get_frontmost_app_name_and_bundle`, `daemon.rs:1275`).

Add `get_frontmost_context() -> SourceContext` that extends the existing
AppleScript round-trip to also pull the window title, and — when the bundle id
is a known browser (Safari/Chrome/Arc/etc.) — the active tab URL. Call it at
**copy** time (not paste; paste already reads `get_frontmost_app_name` for
`dest_app`) and thread the result into every `ClipEntry::new`.

Cost: one extended AppleScript call per copy, off the hot path (runs in the
persist path, not the keystroke path). No new dependency.

---

## 2. Local recall engine — one function, exact-first

New module `clipd-core/src/recall.rs`. One entry point the palette calls:

```rust
pub struct RecallQuery {
    pub text: String,                  // free text after filters are stripped
    pub since: Option<DateTime<Utc>>,  // parsed from "yesterday", "today", "last week"
    pub app: Option<String>,           // parsed from "from chrome", "in excel"
    pub content_type: Option<ContentType>, // parsed from "code"/"link"/"email"
}

pub struct RecallHit { pub clip: ClipEntry, pub score: f64, pub exact: bool }

pub fn parse(input: &str) -> RecallQuery;                 // lightweight, local, no AI
pub fn recall(store: &ClipStore, q: &RecallQuery, k: usize) -> Vec<RecallHit>;
```

**`parse`** pulls structured filters out of the text before ranking — this is the
"auth bug from yesterday = 3 queries" point, all local:
- `yesterday` / `today` / `this week` / `last week` → `since`
- `from <app>` / `in <app>` → `app`
- `code` / `link` / `url` / `email` / `path` → `content_type`
- a leading `>` → reserved for "act" mode (Tier 2); ignored in Tier 1
- everything else → `text`

**`recall`** ranks by combining the two local signals, exact first:
1. Apply `since` / `app` / `content_type` as SQL filters via the existing
   `SearchFilters` (`models.rs:191`) — these map 1:1 to `ClipStore::search`.
2. If `text` is non-empty, run FTS5 (`store.search`) for **exact** hits and
   build a `TfIdfIndex` over the filtered candidate set (cap ~500 recent) for
   **semantic** hits.
3. Merge: exact FTS matches rank above TF-IDF-only matches (`exact: true`),
   then by score, then recency. Dedup by `id`.
4. If `text` is empty (just filters, or nothing typed) → most-recent-first.

No network, no key. `TfIdfIndex::build`/`search` already exist and are tested
(`semantic.rs:182-211`). This is assembly, not new algorithms.

> Optional later: when `is_embedding_available()`, re-rank the top ~50 with
> cosine over stored embeddings (`search_embeddings`, `embedding.rs:149`). Pure
> add-on; Tier 1 does not depend on it.

---

## 3. The palette — `⌃⌥Space`, type → preview → Enter

### 3.1 Hotkey: evolve, don't add

`⌃⌥Space` already opens the slot-memory HUD (`HotkeyTick::SlotMemoryHud`,
handled at `daemon.rs:461`, emitted from the grab handler). **Evolve it into the
palette** rather than burning a new chord: it already means "show me my memory."

### 3.2 Interaction (recall and act are separate — no intent parsing)

```
⌃⌥Space
  → palette opens, shows most-recent clips with label + preview + source
type "auth"            → live re-rank (exact FTS first, then TF-IDF)
type "auth from chrome"→ filtered + ranked
↑/↓ to move · the highlighted row shows an EXACT, verbatim preview
Enter                  → paste that clip (the exact bytes, instant, no AI)
⎋                      → dismiss
```

Tier 1 stops at "paste the exact clip." The second-keystroke action menu
(`Paste · Clean · Summarize`) is the Tier 2 hook and is intentionally absent
here so nothing risky can happen yet.

### 3.3 Two implementation options

- **v0 (days, reuses proven code):** model it on `open_slot_picker`
  (`daemon.rs:1845`), which already builds an `osascript "choose from list"` with
  previews. Seed the list with `recall(store, parse(""), 50)` (recent), let the
  native dialog's type-to-filter narrow it, paste the chosen `id`. Limitation:
  the native list filters a *fixed* set by substring — no live semantic re-rank.
  Acceptable to ship and learn.
- **v1 (the real thing):** a native always-available panel with a live text
  field that re-calls `recall()` on each keystroke and renders the HUD-style
  list we already built (the tagged `STYLE list` rows in `clipd-hud.swift`).
  Either extend `clipd-hud` into an interactive panel, or add a "palette" window
  to the egui `clipd-gui`. This is where live re-ranking + exact-preview badges
  live.

### 3.4 Result rows reuse the HUD we just built

Each row: source glyph · `label()` · preview · an **`exact`** badge (proves it's
verbatim). The `STYLE list` / `ROW` payload format already supports
badge+icon+preview rows — extend it with a source chip and an exact marker.

---

## 4. Named/semantic primary, slots secondary

- **Naming:** add `HotkeyTick::NameLastClip` (a chord, e.g. `⌃⌥N`) → small input
  → writes `clips.name`. Palette rows then show the name as the primary label.
  `paste api` works because `name` is FTS-indexed and exact-matched first.
- **Slots stay, demoted:** numbered/letter slots keep working exactly as today
  (they're the stable addressing layer). The palette becomes the *primary* way
  in; slots are the muscle-memory shortcut for power users. No slot code changes.
- **AI labels (later, advisory only):** `label()` may show a heuristic/AI guess
  ("API token") when `name` is unset — but it is display-only, never written to
  `name`, never an address. The A=API-as-slot trap is structurally impossible.

---

## 5. Privacy — reuse `privacy.rs`, make state visible

- **Capture filter already exists:** `should_skip_clip` (`privacy.rs:146`) and
  `is_excluded_app` (`privacy.rs:106`) already drop secrets and password-manager
  apps. Source capture must run through the **same** gate: never store a
  `source_title`/`source_url` for an excluded app, and run `detect_sensitive`
  over titles/URLs too (a window title can be "Chase — Balance").
- **Badges in the palette:** each row shows one of `Local` (default) / `AI`
  (only if a transform touched it — Tier 2) / `Redacted` (matched
  `detect_sensitive`; preview uses `SensitiveMatch::redacted_preview`,
  `privacy.rs:32`). Tier 1 rows are all `Local`.
- **AI stays gated:** Tier 1 makes **zero** network calls. The first cloud call
  only happens in Tier 2, on an explicit action, after secret redaction.

---

## 6. Per-app behavior — cheap, built on existing detection

The daemon already branches on the frontmost app (`is_terminal_frontmost`,
`daemon.rs:1310`, used to suppress Ctrl+C in terminals/IDEs). Generalize that
into an app→profile lookup used for **defaults only** (never silent transforms):

```
VS Code / JetBrains  → palette default action = paste as-is; Tier 2: code transforms
Terminal             → Tier 2: clean log / command
Excel / Numbers      → Tier 2 default: values-only paste, table actions
Browser              → capture URL (already specced); Tier 2: summarize/extract
```

Tier 1 uses this only to (a) capture browser URLs and (b) order the action menu
later. No behavior change to plain paste.

---

## 7. Smallest shippable v0 (build order)

1. **Schema + capture** (§1.2, §1.3): add columns, `get_frontmost_context`, thread
   `SourceContext` into `ClipEntry::new`. Ship — clips now remember source.
   *Verifiable immediately via `clipd stats` / the TUI.*
2. **`recall.rs`** (§2): `parse` + `recall` over FTS5 + `TfIdfIndex`. Unit-test
   against an in-memory `ClipStore` (pattern at `store.rs:530`). No UI yet.
3. **Palette v0** (§3.3): `⌃⌥Space` → osascript picker seeded by `recall()`.
   Ship the "type → Enter → paste exact" loop.
4. **Naming** (§4): `⌃⌥N` to set `clips.name`; show names in the palette.
5. **Privacy pass** (§5): route source capture through `should_skip_clip`; add
   `Local`/`Redacted` badges to rows.

Stop there. Palette v1 (native live re-rank), per-app action menus, and any AI
transform are the next milestone, not this one.

---

## 8. Risks / decisions to confirm

- **FTS rebuild on upgrade:** changing the `clips_fts` column set requires a
  one-time rebuild on existing DBs. Gate behind a `schema_version` row; on
  mismatch, `DROP` + recreate the FTS table and re-populate from `clips`.
- **Palette input on macOS:** v0 osascript can't live-re-rank; v1 needs a real
  text field (native panel or egui). Confirm which surface owns the palette
  (`clipd-hud` vs `clipd-gui`) before building v1.
- **`⌃⌥Space` overload:** it currently shows the slot-memory HUD. Evolving it
  means that HUD becomes the palette's "empty query" state — confirm that's the
  intended merge (recommended) vs. keeping them separate.
- **Pre-existing bug to fix first:** `ClipStore::search` FTS branch selects 7
  columns (`store.rs:206`, no `slot`) but the row closure reads `row.get(7)` for
  `slot` (`store.rs:249`) — that's an out-of-range read, so **any keyword search
  currently errors at runtime.** Recall depends on this path; fix it as step 0.
```
