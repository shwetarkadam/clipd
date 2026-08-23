use chrono::{Duration, Utc};
use clap::{Parser, Subcommand};
use clipd_core::{
    AskConfig, AskFilters, AskThread, ClipStore, ContentType, SearchFilters, SlotManager,
    MAX_CLIP_SLOT,
};

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

    /// Ask a question about your clipboard history (grounded, with citations)
    Ask {
        /// The question, e.g. "what was that stripe webhook payload?"
        question: Vec<String>,

        /// Continue the most recent conversation instead of starting fresh
        #[arg(short, long)]
        continue_thread: bool,

        /// Only consider clips from this app
        #[arg(short, long)]
        app: Option<String>,

        /// Only consider clips from the last: 1h, 6h, 1d, 7d, 30d
        #[arg(short, long)]
        last: Option<String>,

        /// How many clips to put in front of the model
        #[arg(short = 'k', long, default_value = "8")]
        top_k: usize,

        /// Emit JSON (answer, sources, confidence) instead of prose
        #[arg(long)]
        json: bool,

        /// Retrieve and rank only — never call the API, even if a key is set
        #[arg(long)]
        no_ai: bool,

        /// List saved ask conversations and exit
        #[arg(long)]
        threads: bool,
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

    /// Manage collections — named buckets of clips (e.g. your Cursor prompts)
    Collections {
        #[command(subcommand)]
        action: CollectionsAction,
    },

    /// Securely save a password to a vault (1Password, Bitwarden, or Keychain)
    Vault {
        #[command(subcommand)]
        action: VaultAction,
    },

    /// Manage reusable text snippets (recalled by trigger in the search palette)
    Snippet {
        #[command(subcommand)]
        action: SnippetAction,
    },

    /// Send a clip to your other Mac
    ///
    /// With one other Mac signed into the same Apple ID there's nothing to
    /// choose, so `clipd send` on its own sends what you just copied.
    Send {
        /// Which Mac to send to (name or id). Omit when you only have one.
        target: Option<String>,

        /// Send clip #ID from history instead of the current clipboard
        #[arg(long)]
        id: Option<i64>,

        /// Send these files instead of the clipboard
        #[arg(long, num_args = 1..)]
        file: Vec<std::path::PathBuf>,

        /// Take back the last send, if the other Mac hasn't collected it yet
        #[arg(long)]
        undo: bool,
    },

    /// Pair with another machine on this network (run on both, at the same time)
    Pair,

    /// Stop trusting a paired machine
    Unpair {
        /// Machine to forget (name or id). Omit to list what's paired.
        target: Option<String>,
    },

    /// List the Macs clipd can send to
    Devices,

    /// Show or set the folder clipd syncs through
    ///
    /// Defaults to iCloud Drive, but any folder both machines can see works —
    /// a mounted network share, a USB stick, Dropbox, Syncthing.
    SyncRoot {
        /// Folder to use. Omit to show the current one.
        path: Option<String>,

        /// Go back to the default (iCloud Drive)
        #[arg(long)]
        reset: bool,
    },

    /// Check for updates (or update in-place)
    Update,
}

#[derive(Subcommand)]
enum SnippetAction {
    /// Add or update a snippet. Body comes from --body or stdin.
    Add {
        /// Short trigger keyword (e.g. "sig")
        trigger: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        body: Option<String>,
    },
    /// List all snippets
    List,
    /// Remove a snippet by trigger
    Rm { trigger: String },
}

#[derive(Subcommand)]
enum VaultAction {
    /// List which vault backends are usable on this machine
    Targets,
    /// Save a password to a vault. Reads the password from stdin if --password is omitted.
    Save {
        /// Which vault: 1password | bitwarden | keychain
        #[arg(long, default_value = "keychain")]
        to: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        username: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        notes: Option<String>,
        /// The password. If omitted, read from stdin (safer — keeps it out of shell history).
        #[arg(long)]
        password: Option<String>,
    },
    /// List the passwords clipd has saved to the system store
    List,
    /// Copy a saved password to the clipboard (auto-clears after 30s)
    Copy {
        /// Row number from `clipd vault list`, or part of the entry's name
        which: String,
        /// Print the password to stdout instead of copying it
        #[arg(long)]
        show: bool,
        /// Leave the password on the clipboard instead of clearing it
        #[arg(long)]
        keep: bool,
    },
    /// Give a saved password a meaningful name
    Rename {
        /// Row number from `clipd vault list`, or part of the entry's name
        which: String,
        /// The new name
        name: String,
    },
    /// Delete a saved password from the system store
    Rm {
        /// Row number from `clipd vault list`, or part of the entry's name
        which: String,
    },
}

#[derive(Subcommand)]
enum CollectionsAction {
    /// Create a collection; --app auto-routes copies made while that app is frontmost
    New {
        name: String,
        #[arg(long)]
        app: Option<String>,
    },
    /// List collections
    List,
    /// Show a collection's items
    Show { name: String },
    /// Add a clip to a collection (defaults to the most recent clip)
    Add {
        name: String,
        #[arg(long)]
        id: Option<i64>,
    },
    /// Remove a clip from a collection by clip id
    Remove { name: String, id: i64 },
    /// Export a collection to Markdown (stdout)
    Export { name: String },
    /// Delete a collection (clips themselves are kept)
    Delete { name: String },
    /// AI: print an improved version of a saved prompt (needs API key)
    Refine { name: String, id: i64 },
    /// AI: turn a saved prompt into a reusable {variable} template (needs API key)
    Template { name: String, id: i64 },
    /// AI: summarize what a collection is about (needs API key)
    Summarize { name: String },
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

        Some(Commands::Ask {
            question,
            continue_thread,
            app,
            last,
            top_k,
            json,
            no_ai,
            threads,
        }) => {
            cmd_ask(
                question.join(" "),
                continue_thread,
                app,
                last,
                top_k,
                json,
                no_ai,
                threads,
            );
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

        Some(Commands::Collections { action }) => {
            cmd_collections(action);
        }

        Some(Commands::Vault { action }) => {
            cmd_vault(action);
        }

        Some(Commands::Snippet { action }) => {
            cmd_snippet(action);
        }

        Some(Commands::Send {
            target,
            id,
            file,
            undo,
        }) => {
            if undo {
                cmd_send_undo();
            } else {
                cmd_send(target.as_deref(), id, &file);
            }
        }

        Some(Commands::Pair) => {
            cmd_pair();
        }

        Some(Commands::Unpair { target }) => {
            cmd_unpair(target.as_deref());
        }

        Some(Commands::Devices) => {
            cmd_devices();
        }

        Some(Commands::SyncRoot { path, reset }) => {
            cmd_sync_root(path.as_deref(), reset);
        }

        Some(Commands::Update) => {
            cmd_update();
        }

        None => {
            // Spawn menu bar UI (or GUI) in background; only start daemon
            // ourselves if neither was available (they handle it internally).
            let ui_launched = spawn_background_ui();
            if !ui_launched {
                launch_daemon_background();
            }
            if let Err(e) = clipd_tui::run_tui() {
                eprintln!("❌ TUI error: {}", e);
                std::process::exit(1);
            }
            clipd_core::release_daemon_lock();
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

fn resolve_collection(store: &ClipStore, name: &str) -> Option<clipd_core::Collection> {
    match store.get_collection_by_name(name) {
        Ok(Some(c)) => Some(c),
        Ok(None) => {
            eprintln!("  ❌ No collection named '{}'.", name);
            None
        }
        Err(e) => {
            eprintln!("  ❌ {}", e);
            None
        }
    }
}

fn ai_on_item(
    store: &ClipStore,
    name: &str,
    id: i64,
    label: &str,
    f: fn(&str, &clipd_core::TransformConfig) -> Result<String, String>,
) {
    let Some(c) = resolve_collection(store, name) else {
        return;
    };
    let items = store.collection_items(c.id).unwrap_or_default();
    let Some(item) = items.iter().find(|it| it.clip_id == id) else {
        eprintln!("  ❌ Clip #{} is not in '{}'.", id, name);
        return;
    };
    let cfg = clipd_core::load_transform_config();
    match f(&item.content, &cfg) {
        Ok(out) => println!("\n── {} ──\n{}\n", label, out),
        Err(e) => eprintln!("  ❌ {}", e),
    }
}

fn cmd_snippet(action: SnippetAction) {
    use std::io::Read;
    let store = open_store();
    match action {
        SnippetAction::Add {
            trigger,
            name,
            body,
        } => {
            let body = match body {
                Some(b) => b,
                None => {
                    let mut buf = String::new();
                    let _ = std::io::stdin().read_to_string(&mut buf);
                    buf.trim_end_matches(['\n', '\r']).to_string()
                }
            };
            if body.trim().is_empty() {
                eprintln!("  ❌ Snippet body is empty (pass --body or pipe via stdin).");
                std::process::exit(1);
            }
            match store.upsert_snippet(trigger.trim(), name.as_deref().unwrap_or("").trim(), &body) {
                Ok(_) => println!("  ✅ Saved snippet ‘{}’", trigger.trim()),
                Err(e) => eprintln!("  ❌ {}", e),
            }
        }
        SnippetAction::List => match store.list_snippets() {
            Ok(s) if s.is_empty() => println!("  ✂️  No snippets yet. Add one: clipd snippet add sig --body \"…\""),
            Ok(snippets) => {
                println!("  ✂️  Snippets:");
                for s in snippets {
                    println!("     {:<12} {}", s.trigger, s.preview());
                }
            }
            Err(e) => eprintln!("  ❌ {}", e),
        },
        SnippetAction::Rm { trigger } => match store.delete_snippet_by_trigger(trigger.trim()) {
            Ok(true) => println!("  ✅ Removed snippet ‘{}’", trigger.trim()),
            Ok(false) => println!("  (no snippet with trigger ‘{}’)", trigger.trim()),
            Err(e) => eprintln!("  ❌ {}", e),
        },
    }
}

fn cmd_vault(action: VaultAction) {
    use clipd_core::{available_targets, save_secret, SecretEntry, VaultTarget};
    use std::io::Read;

    match action {
        VaultAction::Targets => {
            let available = available_targets();
            println!("  🔐 Vault backends:");
            for t in VaultTarget::ALL {
                let mark = if available.contains(&t) { "✅" } else { "—" };
                println!("     {} {}", mark, t.label());
            }
            if available.is_empty() {
                println!("\n  No vault CLIs found. Install `op` (1Password) or `bw` (Bitwarden);");
                println!("  the macOS Keychain works out of the box on macOS.");
            }
        }
        VaultAction::Save {
            to,
            title,
            username,
            url,
            notes,
            password,
        } => {
            let target = match VaultTarget::from_id(&to) {
                Some(t) => t,
                None => {
                    eprintln!("  ❌ Unknown vault '{}'. Use: 1password | bitwarden | keychain", to);
                    std::process::exit(1);
                }
            };
            // Read the password from stdin when not passed as a flag — keeps it
            // out of shell history and the process table.
            let password = match password {
                Some(p) => p,
                None => {
                    let mut buf = String::new();
                    if std::io::stdin().read_to_string(&mut buf).is_err() {
                        eprintln!("  ❌ Failed to read password from stdin.");
                        std::process::exit(1);
                    }
                    buf.trim_end_matches(['\n', '\r']).to_string()
                }
            };

            let entry = SecretEntry {
                title: title.unwrap_or_default(),
                username: username.unwrap_or_default(),
                password,
                url: url.unwrap_or_default(),
                notes: notes.unwrap_or_default(),
            };

            match save_secret(target, &entry) {
                Ok(msg) => println!("  ✅ {}", msg),
                Err(e) => {
                    eprintln!("  ❌ {}", e);
                    std::process::exit(1);
                }
            }
        }

        VaultAction::List => {
            let secrets = vault_list();
            if secrets.is_empty() {
                println!("  🔐 No saved passwords yet.");
                println!("     clipd offers to save one whenever it detects a password on the clipboard.");
                return;
            }
            println!("  🔐 Saved passwords ({}):\n", secrets.len());
            for (i, s) in secrets.iter().enumerate() {
                let when = s
                    .saved_at
                    .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                    .map(|dt| {
                        dt.with_timezone(&chrono::Local)
                            .format("%b %-d, %Y %-I:%M %p")
                            .to_string()
                    })
                    .unwrap_or_else(|| "unknown date".into());
                println!("  {:>3}. {}", i + 1, s.title);
                println!("       {}", when);
            }
            println!("\n  Copy one with:  clipd vault copy <number>");
        }

        VaultAction::Copy { which, show, keep } => {
            let secret = vault_pick(&which);
            let password = match clipd_core::reveal_secret(&secret) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("  ❌ {e}");
                    std::process::exit(1);
                }
            };
            if show {
                println!("{password}");
                return;
            }
            // Blocking, not backgrounded: a wipe thread would die the moment
            // this command exits, leaving the password on the clipboard.
            let clear_after = if keep {
                Some(std::time::Duration::ZERO)
            } else {
                None
            };
            let title = secret.title.clone();
            let announce = || {
                if keep {
                    println!("  ✅ Copied “{title}” — it will stay on the clipboard.");
                } else {
                    println!(
                        "  ✅ Copied “{title}” — paste it now; clearing in {}s (Ctrl+C to keep it).",
                        clipd_core::DEFAULT_CLEAR_AFTER.as_secs()
                    );
                }
            };
            if let Err(e) = clipd_core::copy_secret_blocking(&password, clear_after, announce) {
                eprintln!("  ❌ {e}");
                std::process::exit(1);
            }
            if !keep {
                println!("  🧹 Clipboard cleared.");
            }
        }

        VaultAction::Rename { which, name } => {
            let secret = vault_pick(&which);
            match clipd_core::rename_secret(&secret, &name) {
                Ok(()) => println!("  ✅ Renamed “{}” to “{}”.", secret.title, name.trim()),
                Err(e) => {
                    eprintln!("  ❌ {e}");
                    std::process::exit(1);
                }
            }
        }

        VaultAction::Rm { which } => {
            let secret = vault_pick(&which);
            match clipd_core::forget_secret(&secret) {
                Ok(()) => println!("  ✅ Deleted “{}”.", secret.title),
                Err(e) => {
                    eprintln!("  ❌ {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}

fn vault_list() -> Vec<clipd_core::SecretRef> {
    match clipd_core::list_secrets() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  ❌ {e}");
            std::process::exit(1);
        }
    }
}

/// Resolve a user-supplied selector — a row number from `vault list`, or a
/// case-insensitive fragment of the entry's name — to exactly one secret.
/// Exits with an explanation rather than guessing when the choice is unclear.
fn vault_pick(which: &str) -> clipd_core::SecretRef {
    let secrets = vault_list();
    if secrets.is_empty() {
        eprintln!("  ❌ No saved passwords yet.");
        std::process::exit(1);
    }

    if let Ok(n) = which.trim().parse::<usize>() {
        return match secrets.get(n.wrapping_sub(1)) {
            Some(s) if n >= 1 => s.clone(),
            _ => {
                eprintln!(
                    "  ❌ There's no row {n} — `clipd vault list` shows {}.",
                    match secrets.len() {
                        1 => "1 password".to_string(),
                        n => format!("{n} passwords"),
                    }
                );
                std::process::exit(1);
            }
        };
    }

    let needle = which.trim().to_lowercase();
    let matches: Vec<_> = secrets
        .iter()
        .filter(|s| s.title.to_lowercase().contains(&needle))
        .collect();
    match matches.len() {
        1 => matches[0].clone(),
        0 => {
            eprintln!("  ❌ Nothing saved matches “{which}”. Try `clipd vault list`.");
            std::process::exit(1);
        }
        n => {
            eprintln!("  ❌ “{which}” matches {n} saved passwords:");
            for m in &matches {
                eprintln!("       {}", m.title);
            }
            eprintln!("     Use the row number from `clipd vault list` instead.");
            std::process::exit(1);
        }
    }
}

fn cmd_collections(action: CollectionsAction) {
    let store = open_store();
    match action {
        CollectionsAction::New { name, app } => match store.create_collection(&name, app.as_deref())
        {
            Ok(_) => match app {
                Some(a) => println!(
                    "  ✅ Created '{}' — copies made while {} is frontmost auto-file here",
                    name, a
                ),
                None => println!("  ✅ Created collection '{}'", name),
            },
            Err(e) => eprintln!("  ❌ Could not create (name taken?): {}", e),
        },
        CollectionsAction::List => match store.list_collections() {
            Ok(cs) if cs.is_empty() => println!(
                "  📂 No collections yet. Try: clipd collections new \"Cursor prompts\" --app Cursor"
            ),
            Ok(cs) => {
                println!("  📂 Collections:");
                for c in cs {
                    let app = c
                        .source_app
                        .map(|a| format!("  ⟲ {}", a))
                        .unwrap_or_default();
                    println!("    {} — {} items{}", c.name, c.item_count, app);
                }
            }
            Err(e) => eprintln!("  ❌ {}", e),
        },
        CollectionsAction::Show { name } => {
            if let Some(c) = resolve_collection(&store, &name) {
                let items = store.collection_items(c.id).unwrap_or_default();
                println!("  📂 {} ({} items)", c.name, items.len());
                for (i, it) in items.iter().enumerate() {
                    println!("    {:>3}. [#{}] {}", i + 1, it.clip_id, it.preview);
                }
            }
        }
        CollectionsAction::Add { name, id } => {
            if let Some(c) = resolve_collection(&store, &name) {
                let clip_id = match id {
                    Some(i) => i,
                    None => match store.get_recent(1) {
                        Ok(v) if !v.is_empty() => v[0].id,
                        _ => {
                            eprintln!("  ❌ No recent clip to add.");
                            return;
                        }
                    },
                };
                match store.add_clip_to_collection(c.id, clip_id) {
                    Ok(_) => println!("  ✅ Added clip #{} to '{}'", clip_id, c.name),
                    Err(e) => eprintln!("  ❌ {}", e),
                }
            }
        }
        CollectionsAction::Remove { name, id } => {
            if let Some(c) = resolve_collection(&store, &name) {
                let _ = store.remove_collection_item(c.id, id);
                println!("  ✅ Removed clip #{} from '{}'", id, c.name);
            }
        }
        CollectionsAction::Export { name } => {
            if let Some(c) = resolve_collection(&store, &name) {
                let items = store.collection_items(c.id).unwrap_or_default();
                println!("# {}\n", c.name);
                for it in &items {
                    println!("- {}\n", it.content.replace('\n', "\n  "));
                }
            }
        }
        CollectionsAction::Delete { name } => {
            if let Some(c) = resolve_collection(&store, &name) {
                let _ = store.delete_collection(c.id);
                println!("  ✅ Deleted collection '{}'", c.name);
            }
        }
        CollectionsAction::Refine { name, id } => {
            ai_on_item(&store, &name, id, "Refined prompt", clipd_core::refine_prompt);
        }
        CollectionsAction::Template { name, id } => {
            ai_on_item(&store, &name, id, "Template", clipd_core::make_template);
        }
        CollectionsAction::Summarize { name } => {
            if let Some(c) = resolve_collection(&store, &name) {
                let items = store.collection_items(c.id).unwrap_or_default();
                let cfg = clipd_core::load_transform_config();
                match clipd_core::summarize_collection(&items, &cfg) {
                    Ok(s) => println!("\n{}\n", s),
                    Err(e) => eprintln!("  ❌ {}", e),
                }
            }
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

    match SlotManager::persistent_default().and_then(|slots| slots.get_slot(slot)) {
        Ok(Some(content)) => print!("{}", content),
        Ok(None) => eprintln!("❌ Slot {} is empty", slot),
        Err(e) => eprintln!("❌ Failed to read slot {}: {}", slot, e),
    }
}

fn cmd_slots() {
    println!("  🎰 Slot contents:");
    println!("  {}", "─".repeat(50));
    match SlotManager::persistent_default().and_then(|slots| slots.list_slots()) {
        Ok(slots) => {
            for (slot, content) in slots.into_iter().filter(|(slot, _)| *slot > 0) {
                let label = if (31..=56).contains(&slot) {
                    ((b'A' + slot - 31) as char).to_string()
                } else {
                    slot.to_string()
                };
                println!(
                    "  {:>2}  {}",
                    label,
                    truncate(&content.replace('\n', " "), 40)
                );
            }
        }
        Err(e) => eprintln!("❌ Failed to read slots: {}", e),
    }
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

// ── Ask ──

#[allow(clippy::too_many_arguments)]
fn cmd_ask(
    question: String,
    continue_thread: bool,
    app: Option<String>,
    last: Option<String>,
    top_k: usize,
    json: bool,
    no_ai: bool,
    threads: bool,
) {
    let store = open_store();

    if threads {
        list_ask_threads(&store);
        return;
    }

    if question.trim().is_empty() {
        eprintln!("  Usage: clipd ask \"what was that postgres connection string?\"");
        eprintln!("         clipd ask --continue-thread \"and the one before it?\"");
        eprintln!("         clipd ask --threads    List saved conversations");
        std::process::exit(2);
    }

    let filters = AskFilters {
        source_app: app,
        since: last.and_then(|l| parse_duration(&l).map(|d| Utc::now() - d)),
    };
    let cfg = AskConfig {
        top_k: top_k.clamp(1, 40),
        ..Default::default()
    };

    // --no-ai forces the local path by handing `ask` an empty key, so the two
    // modes go through exactly one code path rather than diverging here.
    let api = if no_ai {
        clipd_core::TransformConfig {
            api_key: None,
            ..clipd_core::load_transform_config()
        }
    } else {
        clipd_core::load_transform_config()
    };

    let mut thread = if continue_thread {
        AskThread::resume_latest(&store)
    } else {
        AskThread::new()
    };

    if continue_thread && !thread.turns.is_empty() && !json {
        println!(
            "  ↩︎  Continuing conversation ({} previous turn{})",
            thread.turns.len(),
            if thread.turns.len() == 1 { "" } else { "s" }
        );
        println!();
    }

    let answer = match clipd_core::ask(&store, &question, &thread, &filters, &cfg, &api) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("❌ {}", e);
            std::process::exit(1);
        }
    };

    if json {
        println!("{}", ask_json(&answer));
    } else {
        println!("{}", answer.render());
    }

    // Retrieval-only runs produce no answer worth replaying to a model, so
    // they don't open or extend a thread.
    if !answer.retrieval_only {
        thread.record(&store, &answer);
    }
}

fn list_ask_threads(store: &ClipStore) {
    match store.list_ask_threads(20) {
        Ok(threads) if threads.is_empty() => {
            println!("  No saved conversations yet. Start one with `clipd ask \"...\"`.");
        }
        Ok(threads) => {
            println!("  💬 Ask conversations\n");
            for (id, title, updated, turns) in threads {
                println!(
                    "  #{:<4} {:<48} {} turn{}, {}",
                    id,
                    title,
                    turns,
                    if turns == 1 { "" } else { "s" },
                    format_relative_time(&updated)
                );
            }
            println!("\n  Resume the most recent: clipd ask --continue-thread \"...\"");
        }
        Err(e) => eprintln!("❌ Failed to list conversations: {}", e),
    }
}

fn ask_json(answer: &clipd_core::AskAnswer) -> String {
    let value = serde_json::json!({
        "question": answer.question,
        "answer": answer.answer,
        "confidence": answer.confidence.label(),
        "retrieval_only": answer.retrieval_only,
        "withheld_count": answer.withheld_count,
        "invalid_citations": answer.invalid_citations,
        "estimated_prompt_tokens": answer.estimated_prompt_tokens,
        "usage": answer.usage,
        "sources": answer.sources,
        "retrieved": answer.retrieved.iter().map(|r| serde_json::json!({
            "clip_id": r.clip.id,
            "preview": r.clip.preview,
            "source_app": r.clip.source_app,
            "timestamp": r.clip.timestamp,
            "fused_score": r.fused_score,
            "matched_by": r.matched_by(),
            "retriever_count": r.retriever_count(),
            "withheld": r.withheld,
        })).collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
}

fn cmd_clear(slot: Option<u8>, all: bool, before: Option<String>) {
    let store = open_store();

    if let Some(s) = slot {
        match SlotManager::persistent_default().and_then(|slots| slots.clear_slot(s)) {
            Ok(()) => println!("  🗑️  Slot {} cleared", s),
            Err(e) => eprintln!("❌ Failed to clear slot {}: {}", s, e),
        }
    } else if all {
        match store.clear_all() {
            Ok(count) => println!("  🗑️  Cleared {} clips from history", count),
            Err(e) => eprintln!("❌ Failed to clear: {}", e),
        }
        // Questions and answers quote the clips they were about, so wiping
        // history without wiping conversations would leave copies behind.
        match store.clear_ask_threads() {
            Ok(n) if n > 0 => println!("  🗑️  Cleared {} ask conversation(s)", n),
            Ok(_) => {}
            Err(e) => eprintln!("❌ Failed to clear ask conversations: {}", e),
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

/// `clipd send` — put a clip on another Mac.
fn cmd_send(target: Option<&str>, id: Option<i64>, files: &[std::path::PathBuf]) {
    let clip = match build_clip_to_send(id, files) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ {e}");
            std::process::exit(1);
        }
    };

    match clipd_core::sync::send_clip(&clip, target) {
        Ok(device) => {
            println!("📤 Sent to {} — {}", device.name, clip.preview);
            // The other Mac's daemon has to be running to collect it; say so
            // once rather than leaving someone watching a folder.
            println!("   It'll appear in clipd on that Mac within a few seconds.");
            println!("   Wrong Mac? `clipd send --undo` takes it back.");
        }
        Err(e) => {
            eprintln!("❌ {e}");
            std::process::exit(1);
        }
    }
}

/// Work out what `clipd send` is actually sending: explicit files, a clip from
/// history, or whatever is on the clipboard right now.
fn build_clip_to_send(
    id: Option<i64>,
    files: &[std::path::PathBuf],
) -> Result<clipd_core::ClipEntry, String> {
    if !files.is_empty() {
        if let Some(missing) = files.iter().find(|p| !p.exists()) {
            return Err(format!("{} doesn't exist.", missing.display()));
        }
        let refs = clipd_core::save_files(files);
        if refs.is_empty() {
            return Err("None of those files could be read.".into());
        }
        return Ok(clipd_core::ClipEntry::new_files(refs, None));
    }

    if let Some(id) = id {
        let store = ClipStore::new(&ClipStore::default_path())
            .map_err(|e| format!("Couldn't open the clip store: {e}"))?;
        return store
            .get_by_id(id)
            .map_err(|_| format!("No clip #{id} in history."));
    }

    // Nothing named: send what's on the clipboard. Files first, for the same
    // reason the watcher checks them first — a Finder copy also carries text.
    let copied = clipd_core::clipboard_read_file_urls();
    if !copied.is_empty() {
        let refs = clipd_core::save_files(&copied);
        if refs.is_empty() {
            return Err("The copied files couldn't be read.".into());
        }
        return Ok(clipd_core::ClipEntry::new_files(refs, None));
    }

    match clipd_core::clipboard_read_text() {
        Some(text) if !text.trim().is_empty() => Ok(clipd_core::ClipEntry::new(text, None, None)),
        _ => Err("Nothing on the clipboard to send. Copy something first, \
                  or use --id to send from history."
            .into()),
    }
}

/// `clipd send --undo` — take back the last send.
fn cmd_send_undo() {
    match clipd_core::sync::recall_last() {
        Ok((last, true)) => {
            println!("↩️  Took it back before {} picked it up.", last.device_name);
        }
        Ok((last, false)) => {
            println!("⚠️  Too late — {} already has it.", last.device_name);
            println!("   Delete it from clipd on that Mac if it shouldn't be there.");
        }
        Err(e) => {
            eprintln!("❌ {e}");
            std::process::exit(1);
        }
    }
}

/// `clipd pair` — trust another machine on this network.
fn cmd_pair() {
    use std::io::Write;

    println!("Pairing this machine: {}", clipd_core::device_name());
    println!();
    println!("Run `clipd pair` on the other machine too, now.");
    println!("Looking for it on the network…");

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let offer = match clipd_core::lan_pair::discover_and_exchange(stop) {
        Ok(o) => o,
        Err(e) => {
            eprintln!();
            eprintln!("❌ {e}");
            std::process::exit(1);
        }
    };

    println!();
    println!("  Found: {}", offer.name);
    println!();
    println!("      ┌────────────┐");
    println!("      │   {}   │", offer.confirmation_code);
    println!("      └────────────┘");
    println!();
    // The comparison is the security. Say so, rather than implying the number
    // is a password to be typed somewhere.
    println!("This exact code must be showing on {} right now.", offer.name);
    println!("If the two don't match, something is intercepting — answer no.");
    println!();
    print!("Do the codes match? [y/N] ");
    let _ = std::io::stdout().flush();

    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        eprintln!("❌ Couldn't read your answer — nothing was paired.");
        std::process::exit(1);
    }
    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        println!();
        println!("Cancelled. Nothing was paired.");
        std::process::exit(1);
    }

    match offer.accept() {
        Ok(()) => {
            println!();
            println!("✅ Paired with {}.", offer.name);
            println!("   Copy something, then press Ctrl+Shift+S to send it there.");
        }
        Err(e) => {
            eprintln!("❌ {e}");
            std::process::exit(1);
        }
    }
}

/// `clipd unpair` — stop trusting a machine.
fn cmd_unpair(target: Option<&str>) {
    let paired = clipd_core::lan_identity::trusted_peers();

    let Some(target) = target.map(str::trim).filter(|t| !t.is_empty()) else {
        if paired.is_empty() {
            println!("No machines are paired with this one.");
            println!("Run `clipd pair` on both machines to pair.");
            return;
        }
        println!("Paired machines:");
        for p in paired.values() {
            println!(
                "  {} · paired {}",
                p.name,
                p.paired_at.format("%-d %b %Y")
            );
        }
        println!();
        println!("Forget one with: clipd unpair <name>");
        return;
    };

    let peer = match clipd_core::lan_identity::resolve_trusted(target) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("❌ {e}");
            std::process::exit(1);
        }
    };

    match clipd_core::lan_identity::forget_peer(&peer.device_id) {
        Ok(true) => {
            println!("✅ Forgot {}.", peer.name);
            println!("   It can no longer send clips here, and this machine can't send to it.");
            println!("   Run `clipd pair` on both to set it up again.");
        }
        // resolve_trusted just found it, so this means something else removed
        // it in between — worth saying rather than claiming success.
        Ok(false) => println!("{} was already forgotten.", peer.name),
        Err(e) => {
            eprintln!("❌ {e}");
            std::process::exit(1);
        }
    }
}

/// `clipd sync-root` — show or set the folder clipd syncs through.
fn cmd_sync_root(path: Option<&str>, reset: bool) {
    if reset {
        match clipd_core::devices::save_sync_root(None) {
            Ok(()) => println!("✅ Back to the default (iCloud Drive)."),
            Err(e) => {
                eprintln!("❌ {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    if let Some(path) = path {
        match clipd_core::devices::save_sync_root(Some(std::path::Path::new(path))) {
            Ok(()) => {
                println!("✅ clipd now syncs through {path}");
                println!("   Set the same folder on your other machine, then run `clipd devices`.");
                println!("   Restart clipd so the daemon picks it up.");
            }
            Err(e) => {
                eprintln!("❌ {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    match clipd_core::sync_root() {
        Some(root) => {
            let source = if std::env::var_os("CLIPD_SYNC_ROOT").is_some() {
                "from CLIPD_SYNC_ROOT"
            } else if clipd_core::devices::load_sync_root().is_some() {
                "chosen with `clipd sync-root <path>`"
            } else {
                "default (iCloud Drive)"
            };
            println!("Syncing through: {}", root.display());
            println!("  {source}");
            if !root.exists() {
                println!("  ⚠️  That folder doesn't exist yet — it'll be created on first use.");
            }
        }
        None => {
            println!("No sync folder set, and iCloud Drive is off.");
            println!();
            println!("Pick any folder both machines can see:");
            println!("  clipd sync-root /Volumes/shared/clipd");
        }
    }
}

/// `clipd devices` — the Macs this one can send to.
fn cmd_devices() {
    let me = clipd_core::this_device();
    println!("This machine: {} ({})", me.name, &me.id[..6]);

    let reachable = clipd_core::sync::reachable_devices();
    if reachable.is_empty() {
        println!();
        println!("No other machines found.");
        println!();
        println!("Over the network: run clipd on the other machine, on the same Wi-Fi,");
        println!("  then `clipd pair` on both.");
        println!("Through a folder: set the same folder on both with `clipd sync-root`.");
        return;
    }

    let trusted = clipd_core::lan_identity::trusted_peers();
    println!();
    println!("Can send to:");
    for r in &reachable {
        let route = match (&r.lan, r.via_folder) {
            (Some(addr), true) => format!("network ({addr}) + folder"),
            (Some(addr), false) => format!("network ({addr})"),
            (None, _) => "folder".to_string(),
        };
        // A machine on the network that hasn't been paired can be seen but not
        // sent to, which is confusing unless it is spelled out.
        let needs_pairing = r.lan.is_some() && !trusted.contains_key(&r.device_id);
        println!("  {} · {route}", r.name);
        if needs_pairing {
            println!("      ⚠️  not paired yet — run `clipd pair` on both machines");
        }
    }

    let sendable: Vec<&clipd_core::sync::Reachable> = reachable
        .iter()
        .filter(|r| r.via_folder || trusted.contains_key(&r.device_id))
        .collect();
    if sendable.len() == 1 {
        println!();
        println!("`clipd send` goes to {} — no need to name it.", sendable[0].name);
    }

    // Keep the last-seen detail available for the folder transport, where
    // "when did that machine last check in" is the only liveness signal.
    if let Some(root) = clipd_core::sync_root() {
        let folder_peers = clipd_core::peers(&root);
        if !folder_peers.is_empty() {
            println!();
            println!("Folder check-ins:");
            for d in &folder_peers {
                let ago = Utc::now() - d.last_seen;
                let seen = if ago < Duration::minutes(2) {
                    "now".to_string()
                } else if ago < Duration::hours(1) {
                    format!("{}m ago", ago.num_minutes())
                } else if ago < Duration::days(1) {
                    format!("{}h ago", ago.num_hours())
                } else {
                    format!("{}d ago", ago.num_days())
                };
                println!("  {} · {seen}", d.name);
            }
        }
    }
}

fn cmd_update() {
    println!("  Current version: {}", CURRENT_VERSION);
    print!("  Checking for updates... ");

    match fetch_latest_version() {
        Some(latest) if version_is_newer(&latest, CURRENT_VERSION) => {
            println!("v{} available!", latest);
            println!();
            println!("  To update, run:");
            println!("    curl -fsSL https://raw.githubusercontent.com/{}/main/install.sh | bash", GITHUB_REPO);
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
/// Spawns the best available background UI:
///   - clipd-ui  (menu bar + daemon + GUI) if present
///   - clipd-gui (GUI + daemon) otherwise
/// Returns true if clipd-ui was launched (it handles the daemon itself).
fn spawn_background_ui() -> bool {
    if let Some(ui_path) = find_ui_binary() {
        let _ = std::process::Command::new(&ui_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        // Give clipd-ui time to start the daemon before we connect
        std::thread::sleep(std::time::Duration::from_millis(400));
        return true;
    }
    if let Some(gui_path) = find_gui_binary() {
        let _ = std::process::Command::new(&gui_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    false
}

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

fn find_ui_binary() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            #[cfg(target_os = "windows")]
            for name in ["clipd-ui.exe", "clipd-ui"] {
                let candidate = dir.join(name);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                let candidate = dir.join("clipd-ui");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(output) = std::process::Command::new("which").arg("clipd-ui").output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    return Some(PathBuf::from(path));
                }
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(output) = std::process::Command::new("where").arg("clipd-ui").output() {
            if output.status.success() {
                let line = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                if !line.is_empty() {
                    return Some(std::path::PathBuf::from(line));
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
