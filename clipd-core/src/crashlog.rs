//! Crash and hang reports: written locally, sent only when a person says so.
//!
//! The rule this module exists to enforce is that **nothing leaves the machine
//! without an explicit click on a report the person can read in full first**.
//! Not "telemetry is on, so crashes count as telemetry" — a separate, per-report
//! decision, made while looking at the exact bytes that would be sent.
//!
//! The second rule is that a report cannot contain clipboard content. That is
//! not a promise made in a comment; it is a property of the data structure.
//! A report is a fixed set of typed fields plus a bounded list of breadcrumbs,
//! and a breadcrumb is a `&'static str` name with a short detail string. There
//! is no field a clip could be put in without someone deliberately adding one,
//! and no path that copies the log file — which is exactly why the log file is
//! not what gets sent. See `describe_for_log` in `privacy` for the other half.

use std::collections::VecDeque;
use std::ffi::CString;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// How many breadcrumbs a report carries. Enough to see the shape of what the
/// process was doing; short enough that a person can actually read the report
/// they are being asked to consent to.
const CRUMB_KEEP: usize = 40;

/// Longest a single breadcrumb detail may be. A cap rather than a convention:
/// it is the backstop for someone one day passing something they shouldn't.
const CRUMB_MAX: usize = 100;

/// How long the main thread may go without a heartbeat before the watchdog
/// calls it a hang. The island repaints at least every 500ms even when idle,
/// and the palette is event-driven but ticks; ten seconds is far past both.
pub const HANG_AFTER: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportKind {
    /// A Rust panic. We have a message and a location.
    Panic,
    /// The main thread stopped servicing its loop while the process lived.
    Hang,
    /// The process was gone at next launch without ever marking a clean exit —
    /// a hard kill, an OOM, or a crash below Rust's level.
    AbnormalExit,
}

impl ReportKind {
    pub fn headline(&self) -> &'static str {
        match self {
            ReportKind::Panic => "clipd hit an internal error",
            ReportKind::Hang => "clipd stopped responding",
            ReportKind::AbnormalExit => "clipd closed unexpectedly",
        }
    }
}

/// Everything that may be sent, and nothing else.
///
/// Every field here is either a number, a fixed string chosen by clipd, or a
/// bounded breadcrumb. Adding a field is the only way to widen what is
/// collected, which is the point: it makes widening a visible code change
/// rather than something that happens by accident in a log line.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub id: String,
    pub kind: ReportKind,
    /// Unix seconds.
    pub when: u64,
    pub version: String,
    pub os: String,
    pub arch: String,
    /// Which binary: "tray", "island", "palette", "hud", "settings".
    pub surface: String,
    /// Panic message and location, or how long the hang lasted. Never content.
    pub detail: String,
    /// How long the process had been up, in seconds.
    pub uptime_secs: u64,
    /// The display arrangement, as origins and sizes. No monitor names.
    pub displays: String,
    pub breadcrumbs: Vec<String>,
}

impl Report {
    /// The report exactly as it would be sent, pretty-printed.
    ///
    /// This is what the consent dialog shows. Not a summary of it, not a
    /// description of it — the bytes. A person cannot meaningfully consent to
    /// "diagnostic information".
    pub fn as_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }
}

// ── breadcrumbs ───────────────────────────────────────────────────────────────

static CRUMBS: Mutex<Option<VecDeque<String>>> = Mutex::new(None);
static STARTED: Mutex<Option<Instant>> = Mutex::new(None);

fn uptime_secs() -> u64 {
    STARTED
        .lock()
        .ok()
        .and_then(|s| *s)
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0)
}

/// Record what the process was doing, for the report a crash would carry.
///
/// `name` is `&'static str` on purpose: it must be a literal in clipd's own
/// source, so the set of things that can appear here is fixed at compile time.
/// `detail` is bounded and must never carry clipboard content — pass a count,
/// a slot number, a phase name, an arrangement. See `describe_for_log` if you
/// need to say something about a clip.
pub fn breadcrumb(name: &'static str, detail: impl AsRef<str>) {
    let detail = detail.as_ref();
    let detail: String = detail.chars().take(CRUMB_MAX).collect();
    let line = if detail.is_empty() {
        format!("[{:>5}s] {name}", uptime_secs())
    } else {
        format!("[{:>5}s] {name}: {detail}", uptime_secs())
    };
    if let Ok(mut guard) = CRUMBS.lock() {
        let buf = guard.get_or_insert_with(VecDeque::new);
        if buf.len() >= CRUMB_KEEP {
            buf.pop_front();
        }
        buf.push_back(line);
    }
}

fn crumbs() -> Vec<String> {
    CRUMBS
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .map(|b| b.into_iter().collect())
        .unwrap_or_default()
}

// ── storage ───────────────────────────────────────────────────────────────────

pub fn reports_dir() -> PathBuf {
    // Overridable so the tests can exercise the real read/write/sweep paths
    // against a scratch directory instead of the reports a person is actually
    // waiting to decide about.
    if let Some(dir) = std::env::var_os("CLIPD_REPORTS_DIR") {
        let dir = PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    let dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("reports");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn running_marker(surface: &str) -> PathBuf {
    reports_dir().join(format!("running-{surface}"))
}

/// Turn every marker whose process is gone into a report.
///
/// A marker holds the pid that wrote it, so a marker belonging to a *live*
/// process — clipd's other surfaces, running right now — is left alone. This
/// is what makes the sweep safe to run from every surface at startup.
fn sweep_orphaned_markers() {
    let Ok(entries) = std::fs::read_dir(reports_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(surface) = name.strip_prefix("running-") else {
            continue;
        };
        let body = std::fs::read_to_string(&path).unwrap_or_default();
        let mut lines = body.lines();
        let head = lines.next().unwrap_or_default();
        // Line two is the arrangement that was live when this process last
        // published one — the desk as it was, not as it is now.
        let arrangement = lines.next().unwrap_or("unknown").trim().to_string();
        let mut parts = head.split_whitespace();
        let pid: Option<u32> = parts.next().and_then(|p| p.parse().ok());
        let version = parts.next().unwrap_or("unknown").to_string();
        // No pid means a marker from an older build: treat it as an orphan,
        // since the alternative is never reporting it.
        if let Some(pid) = pid {
            if pid == std::process::id() || crate::lock::is_process_alive(pid) {
                continue;
            }
        }
        let mut report = new_report(
            ReportKind::AbnormalExit,
            surface,
            format!("{surface} did not exit cleanly (version {version})"),
            0,
        );
        // Not this process's displays — the dead one's.
        report.displays = if arrangement.is_empty() {
            "unknown".into()
        } else {
            arrangement
        };
        // Nor its breadcrumbs: they belong to a process that is still running.
        report.breadcrumbs = Vec::new();
        write(&report);
        let _ = std::fs::remove_file(&path);
    }
}

/// Reports waiting for a decision, oldest first.
pub fn pending() -> Vec<Report> {
    let mut out: Vec<Report> = Vec::new();
    let Ok(entries) = std::fs::read_dir(reports_dir()) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(report) = serde_json::from_str::<Report>(&text) {
                out.push(report);
            }
        }
    }
    out.sort_by_key(|r| r.when);
    out
}

fn path_of(id: &str) -> PathBuf {
    reports_dir().join(format!("{id}.json"))
}

fn write(report: &Report) {
    let _ = std::fs::write(path_of(&report.id), report.as_json());
}

/// Forget a report without sending it. Used by both "Not now"'s eventual
/// cleanup and by an outright refusal — a declined report is deleted, not
/// kept around in the hope of a later yes.
pub fn discard(id: &str) {
    let _ = std::fs::remove_file(path_of(id));
}

// ── the "never ask" preference ────────────────────────────────────────────────

fn never_ask_path() -> PathBuf {
    reports_dir().join("never-ask")
}

pub fn never_ask() -> bool {
    never_ask_path().exists()
}

pub fn set_never_ask(on: bool) {
    if on {
        let _ = std::fs::write(never_ask_path(), b"1");
        for report in pending() {
            discard(&report.id);
        }
    } else {
        let _ = std::fs::remove_file(never_ask_path());
    }
}

// ── building a report ─────────────────────────────────────────────────────────

fn new_report(kind: ReportKind, surface: &str, detail: String, uptime: u64) -> Report {
    Report {
        id: crate::telemetry::report_id(),
        kind,
        when: crate::telemetry::now_unix_pub(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        surface: surface.to_string(),
        detail,
        uptime_secs: uptime,
        displays: display_arrangement(),
        breadcrumbs: crumbs(),
    }
}

/// The display arrangement, as plain geometry.
///
/// Origins and sizes only — never `localizedName`, which is a monitor model
/// and reads like a fingerprint. Carried on every report because a whole class
/// of clipd's window bugs is invisible without it: a panel placed with the
/// wrong screen's coordinates looks like a hang from the outside.
pub fn display_arrangement() -> String {
    ARRANGEMENT
        .lock()
        .ok()
        .and_then(|a| a.clone())
        .unwrap_or_else(|| "unknown".into())
}

static ARRANGEMENT: Mutex<Option<String>> = Mutex::new(None);

/// Publish the current arrangement, e.g. `"0,0,1470x956|-2560,-300,2560x1440"`.
/// Called by whichever surface already measures the displays.
///
/// Also written into this process's marker file, so that if the process dies
/// without a chance to report anything, whoever adopts the marker later can
/// say what the desk looked like *at the time* rather than what it looks like
/// whenever the orphan happens to be noticed. Those are different facts, and
/// on a bug about monitors being plugged in they are the whole question.
pub fn set_display_arrangement(text: impl Into<String>) {
    let text = text.into();
    let changed = ARRANGEMENT
        .lock()
        .map(|mut slot| {
            let changed = slot.as_deref() != Some(text.as_str());
            *slot = Some(text.clone());
            changed
        })
        .unwrap_or(false);
    if changed {
        write_marker(&surface(), &text);
    }
}

fn write_marker(surface_name: &str, arrangement: &str) {
    let _ = std::fs::write(
        running_marker(surface_name),
        format!(
            "{} {}\n{}",
            std::process::id(),
            env!("CARGO_PKG_VERSION"),
            arrangement
        ),
    );
}

// ── install ───────────────────────────────────────────────────────────────────

static SURFACE: Mutex<Option<String>> = Mutex::new(None);
static HEARTBEAT: AtomicU64 = AtomicU64::new(0);
static HANG_RECORDED: AtomicBool = AtomicBool::new(false);

fn surface() -> String {
    SURFACE
        .lock()
        .ok()
        .and_then(|s| s.clone())
        .unwrap_or_else(|| "unknown".into())
}

/// Start crash reporting for this process.
///
/// Does three things: notices whether the *previous* run of this surface ever
/// exited cleanly, installs a panic hook, and drops a marker that
/// `mark_clean_exit` removes. Call it early in `main`, before the work starts.
pub fn install(surface_name: &'static str) {
    if let Ok(mut s) = STARTED.lock() {
        *s = Some(Instant::now());
    }
    if let Ok(mut s) = SURFACE.lock() {
        *s = Some(surface_name.to_string());
    }

    // A marker left behind by a process that is no longer running means that
    // process started and never finished — the cases a panic hook cannot see
    // at all: SIGKILL, an OOM, a crash inside AppKit.
    //
    // Every surface sweeps *all* markers, not just its own. clipd is four or
    // five processes, and the one that died is often not the one that starts
    // again — if the island crashes and is never re-opened, only the palette
    // is left to notice.
    if !never_ask() {
        sweep_orphaned_markers();
    }
    write_marker(surface_name, &display_arrangement());
    catch_terminating_signals(&running_marker(surface_name));

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Write the report first: the default hook may abort the process.
        if !never_ask() {
            let location = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_else(|| "unknown".into());
            let message = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".into());
            let message: String = message.chars().take(400).collect();
            let report = new_report(
                ReportKind::Panic,
                &surface(),
                format!("{message} (at {location})"),
                uptime_secs(),
            );
            write(&report);
        }
        previous_hook(info);
    }));
}

/// Mark this surface as having shut down on purpose. Anything that reaches
/// this point did not crash.
pub fn mark_clean_exit() {
    let _ = std::fs::remove_file(running_marker(&surface()));
}

/// The marker path, pre-encoded so a signal handler can delete it without
/// allocating, locking or formatting anything.
static MARKER_CPATH: OnceLock<CString> = OnceLock::new();

/// Treat a termination signal as a clean exit.
///
/// Without this, only one path in the whole app marked a clean exit: the
/// tray's Quit menu item. Everything else that ends a process politely —
/// logging out, restarting the Mac, Activity Monitor's Quit, an app update
/// replacing the bundle, a `pkill` — arrives as SIGTERM and left the marker
/// behind, so the next launch reported a crash that never happened.
///
/// Which meant every user would have been shown "clipd closed unexpectedly"
/// after every reboot. A crash reporter that cries wolf on a reboot is worse
/// than no crash reporter: people learn to dismiss it, and the one real crash
/// goes with it.
///
/// SIGKILL is deliberately not handled — it cannot be, and a process that was
/// hard-killed genuinely did not exit cleanly.
#[cfg(unix)]
extern "C" fn on_terminating_signal(sig: i32) {
    // Async-signal-safe by construction: `OnceLock::get` is an atomic load and
    // `unlink` is on the POSIX safe list. Nothing here takes a lock — the
    // Mutexes this module uses elsewhere could be held by the interrupted
    // thread, and taking one here would deadlock the shutdown.
    if let Some(path) = MARKER_CPATH.get() {
        unsafe { libc::unlink(path.as_ptr()) };
    }
    // Hand the signal back to the default disposition so the process still
    // dies the way the sender intended.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

#[cfg(unix)]
fn catch_terminating_signals(marker: &std::path::Path) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = CString::new(marker.as_os_str().as_bytes()) else {
        return;
    };
    if MARKER_CPATH.set(c).is_err() {
        return; // already installed
    }
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        unsafe { libc::signal(sig, on_terminating_signal as libc::sighandler_t) };
    }
}

#[cfg(not(unix))]
fn catch_terminating_signals(_marker: &std::path::Path) {}

// ── hang detection ────────────────────────────────────────────────────────────

/// Call once per iteration of the main loop.
pub fn heartbeat() {
    HEARTBEAT.store(uptime_secs(), Ordering::Relaxed);
    HANG_RECORDED.store(false, Ordering::Relaxed);
}

/// Watch the main loop from a side thread and record a report if it stops.
///
/// Only ever *records*. A hung process cannot be trusted to draw a consent
/// dialog, so the report waits on disk for the next launch, when there is a
/// working UI to ask in.
pub fn spawn_watchdog() {
    std::thread::Builder::new()
        .name("clipd-watchdog".into())
        .spawn(|| loop {
            std::thread::sleep(Duration::from_secs(2));
            let now = uptime_secs();
            let last = HEARTBEAT.load(Ordering::Relaxed);
            // `last == 0` means the loop has not run yet — starting up is not
            // hanging.
            if last == 0 || now < last {
                continue;
            }
            let stalled = now - last;
            if stalled >= HANG_AFTER.as_secs()
                && !HANG_RECORDED.swap(true, Ordering::Relaxed)
                && !never_ask()
            {
                let report = new_report(
                    ReportKind::Hang,
                    &surface(),
                    format!("main loop did not tick for {stalled}s"),
                    now,
                );
                write(&report);
            }
        })
        .ok();
}

// ── sending ───────────────────────────────────────────────────────────────────

/// Send one report. **Only ever call this from an explicit user action.**
///
/// Deliberately not gated on the analytics toggle. Those are different
/// decisions: someone who does not want to be counted may still want to hand
/// over the crash they just hit, and someone who is happy to be counted has
/// not thereby agreed to send stack locations. Consent for this is the click
/// that reaches this function, and nothing else.
pub fn send(report: &Report) -> SendOutcome {
    let outcome = crate::telemetry::send_report(report);
    if outcome == SendOutcome::Sent {
        discard(&report.id);
    }
    outcome
}

/// What happened to a report the user asked to send.
///
/// Three outcomes, not a bool, because they call for three different things to
/// be said. Collapsing "no endpoint is configured in this build" into the same
/// failure as "the network is down" told people the server was unreachable
/// when nothing had been attempted at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Sent,
    /// This build has no reporting endpoint compiled in — a local or
    /// self-built binary. Sending is impossible, not merely failing.
    NotConfigured,
    Unreachable,
}

/// Whether this build can send a report at all.
///
/// Used to decide whether to ask. Offering someone a Send button that cannot
/// work, and then blaming the network when they press it, is worse than not
/// asking: it spends their goodwill on nothing.
pub fn can_send() -> bool {
    crate::telemetry::reporting_configured()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Point the module at a scratch directory for the duration of a test.
    /// Serialised, because it is process-wide state.
    fn with_scratch_dir<T>(name: &str, body: impl FnOnce() -> T) -> T {
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("clipd-reports-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("CLIPD_REPORTS_DIR", &dir);
        let out = body();
        std::env::remove_var("CLIPD_REPORTS_DIR");
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    fn sample(id: &str, when: u64) -> Report {
        Report {
            id: id.into(),
            kind: ReportKind::Panic,
            when,
            version: "0.1.0".into(),
            os: "macos".into(),
            arch: "aarch64".into(),
            surface: "palette".into(),
            detail: "boom".into(),
            uptime_secs: 1,
            displays: "0,0,1470x956".into(),
            breadcrumbs: vec![],
        }
    }

    #[test]
    fn declining_a_report_deletes_it_rather_than_keeping_it_for_a_later_ask() {
        with_scratch_dir("decline", || {
            write(&sample("aaa", 10));
            assert_eq!(pending().len(), 1);
            discard("aaa");
            assert!(
                pending().is_empty(),
                "a declined report must not sit on disk hoping for a second ask"
            );
        });
    }

    #[test]
    fn never_ask_clears_what_is_already_waiting() {
        with_scratch_dir("never", || {
            write(&sample("bbb", 10));
            write(&sample("ccc", 20));
            assert_eq!(pending().len(), 2);
            set_never_ask(true);
            assert!(never_ask());
            assert!(
                pending().is_empty(),
                "turning it off must also drop the backlog — otherwise \"never\" \
                 means \"not until next launch\""
            );
            set_never_ask(false);
            assert!(!never_ask());
        });
    }

    #[test]
    fn a_marker_belonging_to_a_live_process_is_left_alone() {
        with_scratch_dir("sweep", || {
            // This very process is alive, so its marker must survive a sweep.
            // Getting this wrong would mean every surface reported every other
            // surface as crashed, every launch.
            let alive = reports_dir().join("running-palette");
            std::fs::write(&alive, format!("{} 0.1.0\n0,0,1470x956", std::process::id())).unwrap();
            // A pid that cannot be running.
            let dead = reports_dir().join("running-island");
            std::fs::write(&dead, "999999 0.1.0\n0,0,2560x1440|-1470,300,1470x956,notch").unwrap();

            sweep_orphaned_markers();

            assert!(alive.exists(), "a live surface must not be reported as crashed");
            assert!(!dead.exists(), "an orphaned marker is consumed");
            let reports = pending();
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].surface, "island");
            assert_eq!(reports[0].kind, ReportKind::AbnormalExit);
            // And it carries the desk the *dead* process was on, not this one's.
            assert_eq!(
                reports[0].displays,
                "0,0,2560x1440|-1470,300,1470x956,notch"
            );
        });
    }

    #[test]
    fn a_breadcrumb_cannot_smuggle_a_clip_in() {
        // The cap is the backstop for the mistake this module is designed to
        // make hard: someone passing content into a diagnostic channel.
        let clip = "hunter2-".repeat(200);
        breadcrumb("test.cap", &clip);
        let line = crumbs().last().cloned().unwrap_or_default();
        let detail = line.split_once(": ").map(|(_, d)| d).unwrap_or("");
        assert_eq!(detail.chars().count(), CRUMB_MAX);
        assert!(line.len() < 200, "a breadcrumb stays readable: {line}");
    }

    #[test]
    fn breadcrumbs_stay_bounded() {
        for i in 0..(CRUMB_KEEP * 3) {
            breadcrumb("test.many", i.to_string());
        }
        assert!(crumbs().len() <= CRUMB_KEEP);
    }

    #[test]
    fn a_report_shows_the_person_exactly_what_would_be_sent() {
        // The consent dialog renders `as_json`, and `send` transmits the same
        // struct. If these ever diverge, consent stops meaning anything.
        let report = Report {
            id: "abc".into(),
            kind: ReportKind::Panic,
            when: 1_700_000_000,
            version: "0.1.0".into(),
            os: "macos".into(),
            arch: "aarch64".into(),
            surface: "island".into(),
            detail: "boom (at src/island.rs:12)".into(),
            uptime_secs: 42,
            displays: "0,0,1470x956".into(),
            breadcrumbs: vec!["[    1s] island.phase: expanded".into()],
        };
        let shown = report.as_json();
        let round: Report = serde_json::from_str(&shown).expect("valid json");
        assert_eq!(round.id, report.id);
        assert_eq!(round.detail, report.detail);
        assert_eq!(round.breadcrumbs, report.breadcrumbs);
        // And the thing a reader most needs to see is legible in it.
        assert!(shown.contains("island"));
        assert!(shown.contains("boom"));
    }

    #[test]
    fn every_field_on_a_report_is_one_clipd_chose() {
        // A guard on the shape of the payload: if a field is added, this test
        // fails and whoever added it has to decide, deliberately, that it is
        // safe to put in front of a person as "this is all we send".
        let report = Report {
            id: String::new(),
            kind: ReportKind::Hang,
            when: 0,
            version: String::new(),
            os: String::new(),
            arch: String::new(),
            surface: String::new(),
            detail: String::new(),
            uptime_secs: 0,
            displays: String::new(),
            breadcrumbs: vec![],
        };
        let json: serde_json::Value = serde_json::from_str(&report.as_json()).unwrap();
        let mut keys: Vec<&str> = json.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "arch",
                "breadcrumbs",
                "detail",
                "displays",
                "id",
                "kind",
                "os",
                "surface",
                "uptime_secs",
                "version",
                "when",
            ]
        );
    }
}
