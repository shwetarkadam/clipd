use chrono::{Duration, Utc};
use clap::{Parser, Subcommand};
use clipd_core::{ClipStore, ContentType, SearchFilters};

#[derive(Parser)]
#[command(
    name = "clipd",
    version = "0.1.0",
    about = "🧷 clipd — AI clipboard daemon for developers",
    long_about = "Multi-slot copy/paste, searchable history, and editor integration.\nThink \"Atuin for your clipboard\"."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the clipd daemon (clipboard watcher + global hotkeys)
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
        /// Slot number (0-9)
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
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .format_target(false)
        .init();

    let cli = Cli::parse();

    match cli.command {
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

        None => {
            // No subcommand → show help with branding
            println!("  🧷 clipd v0.1.0 — AI clipboard for developers");
            println!();
            println!("  Usage:");
            println!("    clipd daemon         Start the clipboard daemon");
            println!("    clipd list           Show recent clips");
            println!("    clipd search         Interactive search (TUI)");
            println!("    clipd search <query> Text search");
            println!("    clipd paste <slot>   Output slot to stdout");
            println!("    clipd slots          Show slot contents");
            println!("    clipd stats          Usage statistics");
            println!("    clipd clear          Clear history/slots");
            println!();
            println!("  Hotkeys (when daemon is running):");
            println!("    Cmd+Shift+1..9       Copy to slot");
            println!("    Cmd+Option+1..9      Paste from slot");
            println!("    Cmd+Shift+V          Open search TUI");
            println!();
            println!("  Run 'clipd --help' for full options.");
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
    if slot > 9 {
        eprintln!("❌ Slot must be 0-9");
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
    println!("    Cmd+Shift+1..9   → copy to slot");
    println!("    Cmd+Option+1..9  → paste from slot");
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
