//! Take the tracking off a link as it is copied.
//!
//! Share a link and you usually share a tail of `utm_source=…&fbclid=…` that
//! exists to tell an ad network where the click came from. It does nothing
//! for the person you send it to, makes the link longer and uglier, and quietly
//! reports on both of you. So when a single link is copied, clipd removes the
//! parameters that are *only* tracking, and puts the clean link back on the
//! clipboard — ⌘V pastes what you meant to share.
//!
//! Conservative on purpose. A parameter goes only if it is unambiguous
//! everywhere (`utm_*`, click ids), or it is a known share-tracker on the one
//! site that adds it (`si` on YouTube, `s`/`t` on X). Anything else — a search
//! query, a video's start time, a page id — is the link, and stays exactly as
//! it was, in its original order.

/// Parameters that are tracking on any site.
const ANYWHERE: &[&str] = &[
    "fbclid", "gclid", "dclid", "gbraid", "wbraid", "msclkid", "yclid", "twclid", "ttclid",
    "igshid", "mc_cid", "mc_eid", "_hsenc", "_hsmi", "mkt_tok", "li_fat_id", "oly_anon_id",
    "oly_enc_id", "vero_id", "wickedid", "rb_clickid", "s_cid",
];

/// Share-trackers that only mean tracking on particular sites.
const BY_HOST: &[(&[&str], &[&str])] = &[
    (&["youtube.com", "youtu.be", "music.youtube.com"], &["si", "feature", "pp"]),
    (&["open.spotify.com", "spotify.link"], &["si", "context"]),
    (&["x.com", "twitter.com"], &["s", "t", "ref_src", "ref_url"]),
    (&["instagram.com"], &["igsh", "img_index"]),
    (&["linkedin.com"], &["trk", "trackingid", "lipi", "rcm"]),
    (&["amazon.com", "amazon.co.uk", "amazon.de", "amazon.in"], &["ref_", "pd_rd_r", "pd_rd_w", "pd_rd_wg", "pf_rd_p", "pf_rd_r", "psc"]),
];

fn host_matches(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

fn is_tracking(name: &str, host: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("utm_") || ANYWHERE.contains(&lower.as_str()) {
        return true;
    }
    BY_HOST.iter().any(|(domains, params)| {
        domains.iter().any(|d| host_matches(host, d)) && params.contains(&lower.as_str())
    })
}

/// The link with its tracking parameters removed, or `None` when there is
/// nothing to remove (or `text` is not a single http(s) link).
pub fn clean_link(text: &str) -> Option<String> {
    let trimmed = text.trim();
    // One link and nothing else: a paragraph that happens to contain a link is
    // the person's text, not ours to rewrite.
    if trimmed.is_empty() || trimmed.contains(char::is_whitespace) {
        return None;
    }
    let url = url::Url::parse(trimmed).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let query = url.query()?;

    // Work on the raw query so everything kept stays byte-for-byte as it was
    // (encoding, order, a bare `?flag`).
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let name = pair.split('=').next().unwrap_or("");
            !name.is_empty() && !is_tracking(name, &host)
        })
        .collect();
    let removed_any = kept.len() != query.split('&').filter(|p| !p.is_empty()).count();
    if !removed_any {
        return None;
    }

    let mut cleaned = url.clone();
    if kept.is_empty() {
        cleaned.set_query(None);
    } else {
        cleaned.set_query(Some(&kept.join("&")));
    }
    Some(cleaned.to_string())
}

#[cfg(test)]
mod tests {
    use super::clean_link;

    #[test]
    fn utm_and_click_ids_go() {
        assert_eq!(
            clean_link("https://example.com/post?utm_source=x&utm_medium=social&id=42&fbclid=abc").as_deref(),
            Some("https://example.com/post?id=42")
        );
        assert_eq!(
            clean_link("https://shop.example/item?gclid=1").as_deref(),
            Some("https://shop.example/item")
        );
    }

    #[test]
    fn what_the_link_needs_stays_in_order() {
        // Search query and start time are the link.
        assert_eq!(
            clean_link("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=42&si=trackme").as_deref(),
            Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=42")
        );
        assert_eq!(
            clean_link("https://www.google.com/search?q=rust+clipboard&utm_source=chrome").as_deref(),
            Some("https://www.google.com/search?q=rust+clipboard")
        );
    }

    #[test]
    fn host_specific_params_only_go_on_their_host() {
        // `s` is X's share tracker, but on a random site it may be the page.
        assert_eq!(
            clean_link("https://x.com/someone/status/1?s=20&t=abc").as_deref(),
            Some("https://x.com/someone/status/1")
        );
        assert_eq!(clean_link("https://docs.example.com/page?s=install"), None);
        assert_eq!(clean_link("https://www.example.com/watch?v=1&si=x"), None);
    }

    #[test]
    fn untouched_when_there_is_nothing_to_remove() {
        assert_eq!(clean_link("https://example.com/a?b=1"), None);
        assert_eq!(clean_link("https://example.com/"), None);
    }

    #[test]
    fn only_a_single_http_link_is_rewritten() {
        assert_eq!(clean_link("see https://example.com/?utm_source=x for more"), None);
        assert_eq!(clean_link("ftp://example.com/?utm_source=x"), None);
        assert_eq!(clean_link("not a link"), None);
    }

    #[test]
    fn surrounding_whitespace_is_tolerated_and_dropped() {
        assert_eq!(
            clean_link("  https://example.com/x?utm_campaign=y \n").as_deref(),
            Some("https://example.com/x")
        );
    }
}
