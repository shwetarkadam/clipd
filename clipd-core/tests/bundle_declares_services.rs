//! Guards the app bundle against silently losing network discovery.
//!
//! On macOS 15+, browsing for a Bonjour service that is not listed in
//! `NSBonjourServices` returns **nothing at all** — no error, no peers, no log
//! line. A mismatch between the service names in the code and the ones in
//! Info.plist is therefore invisible until someone notices that sending
//! between machines quietly stopped working.
//!
//! So the two are checked against each other here.

#![cfg(target_os = "macos")]

use clipd_core::lan::SERVICE_TYPE;
use clipd_core::lan_pair::PAIR_SERVICE;

/// mdns-sd wants the fully-qualified `_clipd._tcp.local.`; Info.plist wants the
/// bare `_clipd._tcp`.
fn plist_form(service_type: &str) -> String {
    service_type.trim_end_matches("local.").trim_end_matches('.').to_string()
}

fn bundle_script() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("packaging/macos/create-app-bundle.sh");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("couldn't read {}: {e}", path.display()))
}

#[test]
fn info_plist_declares_every_service_the_code_browses_for() {
    let script = bundle_script();
    for service in [SERVICE_TYPE, PAIR_SERVICE] {
        let declared = format!("<string>{}</string>", plist_form(service));
        assert!(
            script.contains(&declared),
            "Info.plist is missing {declared}.\n\
             Without it macOS returns no peers at all, silently — add it to \
             NSBonjourServices in packaging/macos/create-app-bundle.sh."
        );
    }
}

#[test]
fn info_plist_explains_why_local_network_access_is_wanted() {
    let script = bundle_script();
    assert!(
        script.contains("NSLocalNetworkUsageDescription"),
        "macOS 15+ refuses local network access without a usage description, \
         so discovery never runs."
    );
    assert!(
        script.contains("NSBonjourServices"),
        "NSBonjourServices is what actually makes a browse return results."
    );
}

#[test]
fn the_plist_form_strips_the_mdns_suffix() {
    assert_eq!(plist_form("_clipd._tcp.local."), "_clipd._tcp");
    assert_eq!(plist_form("_clipd-pair._tcp.local."), "_clipd-pair._tcp");
    // Already-bare names pass through untouched.
    assert_eq!(plist_form("_clipd._tcp"), "_clipd._tcp");
}
