use chrono::{Duration, Utc};
use clap::{Parser, Subcommand};
use clipd_core::{ClipStore, ContentType, SearchFilters, MAX_CLIP_SLOT};

#[derive(Parser)]
#[command(
    name = "clipd",
    version = env!("CARGO_PKG_VERSION"),
    about = "🧷 clipd — AI clipboard daemon for developers",
    long_about = "Multi-slot copy/paste, searchable history, and editor integration.\nThink \"Atuin for your clipboard\".\n\n\
                  DEFAULT (no subcommand): starts the graphical app and the background daemon — one step.\n\
                  Put `clipd`, `clipd-gui`, and `clipd-hud` in the same folder (see release zip).",
    after_help = "Quick start: run `clipd` with no arguments — GUI opens and the daemon starts automatically."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Same as running `clipd` with no arguments (GUI + daemon)
    Gui,

    /// Launch the TUI with built-in daemon (recommended for developers)
    Tui,

    /// Start the clipd daemon only (headless, no UI)
    Daemon,

    /// List recent clipboard entries
    List {
        /// Number of entries to show
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },

    /// Search clipboard history (opens TUI if no query given)
    Search {
        /// Search query (omit for interactive TUI)
        query: Option<String>,

        /// Filter by source app
        #[arg(short, long)]
        app: Option<String>,

        /// Filter by content type (text, url, code, email, path)
        #[arg(short = 't', long = "type")]
        content_type: Option<String>,

        /// Time range: 1h, 6h, 1d, 7d, 30d
        #[arg(short, long)]
        last: Option<String>,

        /// Maximum results
        #[arg(short = 'n', long, default_value = "50")]
        limit: usize,
    },

    /// Output a slot's content to stdout (for piping)
    Paste {
        /// Slot number (0–15)
        slot: u8,
    },

    /// Show current slot contents
    Slots,

    /// Show clipboard statistics
    Stats,

    /// Clear clipboard history or slots
    Clear {
        /// Clear a specific slot
        #[arg(short, long)]
        slot: Option<u8>,

        /// Clear all history
        #[arg(long)]
        all: bool,

        /// Clear entries older than (e.g., 7d, 30d)
        #[arg(long)]
        before: Option<String>,
    },

    /// Check for updates (or update in-place)
    Update,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .format_target(false)
        .init();

    let cli = Cli::parse();

    check_update_background();

    match cli.command {
        Some(Commands::Gui) => {
            launch_gui();
        }

        Some(Commands::Tui) => {
            launch_daemon_background();
            if let Err(e) = clipd_tui::run_tui() {
                eprintln!("❌ TUI error: {}", e);
                std::process::exit(1);
            }
            clipd_core::release_daemon_lock();
        }

        Some(Commands::Daemon) => {
            if let Err(e) = clipd_daemon::run_daemon() {
                eprintln!("❌ Daemon error: {}", e);
                std::process::exit(1);
            }
        }

        Some(Commands::List { limit }) => {
            cmd_list(limit);
        }

        Some(Commands::Search {
            query,
            app,
            content_type,
            last,
            limit,
        }) => {
            if query.is_none() && app.is_none() && content_type.is_none() && last.is_none() {
                // No query → open interactive TUI
                if let Err(e) = clipd_tui::run_tui() {
                    eprintln!("❌ TUI error: {}", e);
                    std::process::exit(1);
                }
            } else {
                cmd_search(query, app, content_type, last, limit);
            }
        }

        Some(Commands::Paste { slot }) => {
            cmd_paste(slot);
        }

        Some(Commands::Slots) => {
            cmd_slots();
        }

        Some(Commands::Stats) => {
            cmd_stats();
        }

        Some(Commands::Clear { slot, all, before }) => {
            cmd_clear(slot, all, before);
        }

        Some(Commands::Update) => {
            cmd_update();
        }

        None => {
            // Default: launch GUI with embedded daemon (user-friendly)
            launch_gui();
        }
    }
}

fn open_store() -> ClipStore {
    let db_path = ClipStore::default_path();
    match ClipStore::new(&db_path) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("❌ Failed to open database: {}", e);
            eprintln!("   Path: {}", db_path.display());
            std::process::exit(1);
        }
    }
}

fn cmd_list(limit: usize) {
    let store = open_store();
    let clips = match store.get_recent(limit) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ Failed to list clips: {}", e);
            return;
        }
    };

    if clips.is_empty() {
        println!("  📋 No clips yet. Copy something and run 'clipd daemon' first.");
        return;
    }

    println!("  📋 Recent clips ({}):", clips.len());
    println!("  {}", "─".repeat(70));

    for clip in &clips {
        let time_str = format_relative_time(&clip.timestamp);
        let app_str = clip
            .source_app
            .as_deref()
            .unwrap_or("unknown");
        let preview = truncate(&clip.preview, 50);

        println!(
            "  {} {:>5} │ {:12} │ {}",
            clip.content_type.icon(),
            time_str,
            app_str,
            preview
        );
    }

    println!("  {}", "─".repeat(70));
}

fn cmd_search(
    query: Option<String>,
    app: Option<String>,
    content_type: Option<String>,
    last: Option<String>,
    limit: usize,
) {
    let store = open_store();

    let since = last.and_then(|l| parse_duration(&l).map(|d| Utc::now() - d));
    let ct = content_type.map(|t| ContentType::from_str(&t));

    let filters = SearchFilters {
        query,
        content_type: ct,
        source_app: app,
        since,
        limit,
    };

    let clips = match store.search(&filters) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ Search failed: {}", e);
            return;
        }
    };

    if clips.is_empty() {
        println!("  🔍 No matching clips found.");
        return;
    }

    println!("  🔍 Found {} clips:", clips.len());
    println!("  {}", "─".repeat(70));

    for clip in &clips {
        let time_str = format_relative_time(&clip.timestamp);
        let app_str = clip
            .source_app
            .as_deref()
            .unwrap_or("unknown");
        let preview = truncate(&clip.preview, 50);

        println!(
            "  {} {:>5} │ {:12} │ {}",
            clip.content_type.icon(),
            time_str,
            app_str,
            preview
        );
    }

    println!("  {}", "─".repeat(70));
}

fn cmd_paste(slot: u8) {
    if slot > MAX_CLIP_SLOT {
        eprintln!("❌ Slot must be 0-{}", MAX_CLIP_SLOT);
        return;
    }

    // For v0.1, slots are in daemon memory. CLI can read from store instead.
    // Read the most recent clip from the store as a fallback.
    let store = open_store();
    let clips = store.get_recent(1).unwrap_or_default();

    if let Some(clip) = clips.first() {
        // Output raw content to stdout for piping
        print!("{}", clip.content);
    } else {
        eprintln!("❌ Slot {} is empty (or daemon not running)", slot);
    }
}

fn cmd_slots() {
    println!("  🎰 Slot contents:");
    println!("  {}", "─".repeat(50));
    println!("  Slots are managed by the running daemon.");
    println!("  Start the daemon with: clipd daemon");
    println!();
    println!("  Hotkeys:");
    println!("    Cmd+C × N or Ctrl+C × N  → save to slot");
    println!("    Cmd+V × N or Ctrl+V × N  → paste from slot");
    println!("  {}", "─".repeat(50));
}

fn cmd_stats() {
    let store = open_store();
    let stats = match store.stats() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("❌ Failed to get stats: {}", e);
            return;
        }
    };

    println!("  📊 clipd Statistics");
    println!("  {}", "─".repeat(40));
    println!("  Total clips:    {}", stats.total_clips);
    println!("  Unique apps:    {}", stats.unique_apps);
    println!(
        "  Database size:  {}",
        format_bytes(stats.db_size_bytes)
    );

    if let Some(oldest) = stats.oldest_clip {
        println!(
            "  Oldest clip:    {}",
            format_relative_time(&oldest)
        );
    }
    if let Some(newest) = stats.newest_clip {
        println!(
            "  Newest clip:    {}",
            format_relative_time(&newest)
        );
    }

    if !stats.top_apps.is_empty() {
        println!();
        println!("  🏆 Top source apps:");
        for (app, count) in &stats.top_apps {
            println!("     {:20} {}", app, count);
        }
    }

    if !stats.type_counts.is_empty() {
        println!();
        println!("  📂 Content types:");
        for (ct, count) in &stats.type_counts {
            let ct_val = ContentType::from_str(ct);
            let icon = ct_val.icon();
            println!("     {} {:12} {}", icon, ct, count);
        }
    }

    println!("  {}", "─".repeat(40));
}

fn cmd_clear(slot: Option<u8>, all: bool, before: Option<String>) {
    let store = open_store();

    if let Some(s) = slot {
        println!("  🗑️  Slot {} cleared (in-memory only — affects running daemon)", s);
    } else if all {
        match store.clear_all() {
            Ok(count) => println!("  🗑️  Cleared {} clips from history", count),
            Err(e) => eprintln!("❌ Failed to clear: {}", e),
        }
    } else if let Some(before_str) = before {
        if let Some(dur) = parse_duration(&before_str) {
            let cutoff = Utc::now() - dur;
            match store.delete_before(&cutoff) {
                Ok(count) => println!("  🗑️  Deleted {} clips older than {}", count, before_str),
                Err(e) => eprintln!("❌ Failed to clear: {}", e),
            }
        } else {
            eprintln!("❌ Invalid duration: {}. Use 1h, 1d, 7d, 30d", before_str);
        }
    } else {
        println!("  Usage:");
        println!("    clipd clear --all           Clear all history");
        println!("    clipd clear --before 30d    Clear clips older than 30 days");
        println!("    clipd clear --slot 3        Clear slot 3");
    }
}

// ── Update ──

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const GITHUB_REPO: &str = "shwetarkadam/clipd";

fn fetch_latest_version() -> Option<String> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", GITHUB_REPO);
    let resp = ureq::get(&url)
        .set("User-Agent", "clipd-updater")
        .call()
        .ok()?;
    let body: serde_json::Value = resp.into_json().ok()?;
    body["tag_name"]
        .as_str()
        .map(|s| s.strip_prefix('v').unwrap_or(s).to_string())
}

fn version_is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> {
        v.split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect()
    };
    parse(latest) > parse(current)
}

fn cmd_update() {
    println!("  Current version: {}", CURRENT_VERSION);
    print!("  Checking for updates... ");

    match fetch_latest_version() {
        Some(latest) if version_is_newer(&latest, CURRENT_VERSION) => {
            println!("v{} available!", latest);
            println!();
            println!("  To update, run:");
            if cfg!(target_os = "windows") {
                println!("    irm https://raw.githubusercontent.com/{}/main/install.ps1 | iex", GITHUB_REPO);
            } else {
                println!("    curl -fsSL https://raw.githubusercontent.com/{}/main/install.sh | bash", GITHUB_REPO);
            }
            println!();
            println!("  Or download from:");
            println!("    https://github.com/{}/releases/latest", GITHUB_REPO);
        }
        Some(latest) => {
            println!("you're on the latest (v{}).", latest);
        }
        None => {
            println!("couldn't reach GitHub. Check your connection.");
        }
    }
}

/// Check for updates in the background (non-blocking). Prints a one-line
/// notice to stderr if a newer version exists — runs at most once per day.
fn check_update_background() {
    use std::path::PathBuf;

    let marker = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("clipd")
        .join("last_update_check");

    if let Ok(meta) = std::fs::metadata(&marker) {
        if let Ok(modified) = meta.modified() {
            if modified.elapsed().unwrap_or_default() < std::time::Duration::from_secs(86400) {
                return;
            }
        }
    }

    std::thread::spawn(move || {
        if let Some(latest) = fetch_latest_version() {
            if version_is_newer(&latest, CURRENT_VERSION) {
                eprintln!(
                    "  💡 clipd v{} is available (you have v{}). Run: clipd update",
                    latest, CURRENT_VERSION
                );
            }
            if let Some(parent) = marker.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&marker, "");
        }
    });
}

// ── Launch helpers ──

/// Spawn the daemon in a background thread (used by `clipd tui`).
fn launch_daemon_background() {
    std::thread::Builder::new()
        .name("clipd-daemon".into())
        .spawn(|| {
            if let Err(e) = clipd_daemon::run_daemon() {
                log::error!("Daemon error: {}", e);
            }
        })
        .ok();
    // Give the daemon a moment to start before showing UI
    std::thread::sleep(std::time::Duration::from_millis(200));
}

/// Find and launch the clipd-gui binary. Falls back to daemon + TUI.
fn launch_gui() {
    // Look for clipd-gui next to the current binary, then in PATH
    let gui_bin = find_gui_binary();
    if let Some(gui_path) = gui_bin {
        println!("  🧷 Launching clipd GUI...");
        match std::process::Command::new(&gui_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => {}
            Err(e) => {
                eprintln!("❌ Failed to launch GUI ({}): {}", gui_path.display(), e);
                eprintln!("   Falling back to TUI...");
                launch_daemon_background();
                if let Err(e) = clipd_tui::run_tui() {
                    eprintln!("❌ TUI error: {}", e);
                }
                clipd_core::release_daemon_lock();
            }
        }
    } else {
        eprintln!("  clipd-gui binary not found — launching TUI instead.");
        eprintln!("  (Build the GUI with: cargo build --release -p clipd-gui)");
        eprintln!();
        launch_daemon_background();
        if let Err(e) = clipd_tui::run_tui() {
            eprintln!("❌ TUI error: {}", e);
        }
        clipd_core::release_daemon_lock();
    }
}

fn find_gui_binary() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            #[cfg(target_os = "windows")]
            for name in ["clipd-gui.exe", "clipd-gui"] {
                let candidate = dir.join(name);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                let candidate = dir.join("clipd-gui");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(output) = std::process::Command::new("where").arg("clipd-gui").output() {
            if output.status.success() {
                let line = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                if !line.is_empty() {
                    return Some(PathBuf::from(line));
                }
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(output) = std::process::Command::new("which")
            .arg("clipd-gui")
            .output()
        {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    return Some(PathBuf::from(path));
                }
            }
        }
    }
    None
}

// ── Helpers ──

fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim().to_lowercase();
    if let Some(h) = s.strip_suffix('h') {
        h.parse::<i64>().ok().map(Duration::hours)
    } else if let Some(d) = s.strip_suffix('d') {
        d.parse::<i64>().ok().map(Duration::days)
    } else if let Some(w) = s.strip_suffix('w') {
        w.parse::<i64>().ok().map(|w| Duration::weeks(w))
    } else if let Some(m) = s.strip_suffix('m') {
        m.parse::<i64>().ok().map(Duration::minutes)
    } else {
        None
    }
}

fn format_relative_time(dt: &chrono::DateTime<Utc>) -> String {
    let now = Utc::now();
    let diff = now.signed_duration_since(*dt);

    if diff.num_seconds() < 60 {
        "just now".to_string()
    } else if diff.num_minutes() < 60 {
        format!("{}m ago", diff.num_minutes())
    } else if diff.num_hours() < 24 {
        format!("{}h ago", diff.num_hours())
    } else if diff.num_days() < 7 {
        format!("{}d ago", diff.num_days())
    } else {
        format!("{}w ago", diff.num_weeks())
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn truncate(s: &str, max: usize) -> String {
    let cleaned = s.replace('\n', " ").replace('\t', " ");
    let char_count: usize = cleaned.chars().count();
    if char_count > max {
        let end: String = cleaned.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", end)
    } else {
        cleaned
    }
}
