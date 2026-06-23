use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SensitiveKind {
    ApiKey(&'static str),
    AwsKey,
    PrivateKey,
    JwtToken,
    GenericSecret,
    CreditCard,
    Ssn,
    ExcludedApp,
}

impl SensitiveKind {
    pub fn label(&self) -> &str {
        match self {
            Self::ApiKey(provider) => provider,
            Self::AwsKey => "AWS Access Key",
            Self::PrivateKey => "Private Key",
            Self::JwtToken => "JWT Token",
            Self::GenericSecret => "Secret/Password",
            Self::CreditCard => "Credit Card",
            Self::Ssn => "SSN",
            Self::ExcludedApp => "Excluded App",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SensitiveMatch {
    pub kind: SensitiveKind,
    pub redacted_preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub excluded_apps: Vec<String>,
    #[serde(default = "default_true")]
    pub detect_api_keys: bool,
    #[serde(default = "default_true")]
    pub detect_credentials: bool,
    #[serde(default = "default_true")]
    pub detect_credit_cards: bool,
    #[serde(default = "default_true")]
    pub detect_ssn: bool,
    #[serde(default)]
    pub custom_skip_patterns: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            excluded_apps: vec![
                "1Password".into(),
                "Bitwarden".into(),
                "KeePassXC".into(),
                "KeePass".into(),
                "LastPass".into(),
                "Dashlane".into(),
                "Enpass".into(),
                "Keeper".into(),
            ],
            detect_api_keys: true,
            detect_credentials: true,
            detect_credit_cards: true,
            detect_ssn: true,
            custom_skip_patterns: Vec::new(),
        }
    }
}

fn config_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clipd")
        .join("privacy.json")
}

pub fn load_privacy_config() -> PrivacyConfig {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_privacy_config(config: &PrivacyConfig) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(config) {
        let _ = std::fs::write(path, json);
    }
}

pub fn is_excluded_app(app: &str, config: &PrivacyConfig) -> bool {
    let app_lower = app.to_lowercase();
    config
        .excluded_apps
        .iter()
        .any(|ex| app_lower.contains(&ex.to_lowercase()))
}

pub fn detect_sensitive(content: &str, config: &PrivacyConfig) -> Vec<SensitiveMatch> {
    if !config.enabled {
        return Vec::new();
    }

    let mut matches = Vec::new();

    if config.detect_api_keys {
        detect_api_keys(content, &mut matches);
    }
    if config.detect_credentials {
        detect_credentials(content, &mut matches);
    }
    if config.detect_credit_cards {
        detect_credit_cards(content, &mut matches);
    }
    if config.detect_ssn {
        detect_ssn(content, &mut matches);
    }

    for pattern in &config.custom_skip_patterns {
        if content.contains(pattern.as_str()) {
            matches.push(SensitiveMatch {
                kind: SensitiveKind::GenericSecret,
                redacted_preview: format!("Custom pattern: {}…", &pattern[..pattern.len().min(20)]),
            });
        }
    }

    matches
}

pub fn should_skip_clip(
    content: &str,
    source_app: Option<&str>,
    config: &PrivacyConfig,
) -> Option<String> {
    if !config.enabled {
        return None;
    }

    if let Some(app) = source_app {
        if is_excluded_app(app, config) {
            return Some(format!("Excluded app: {}", app));
        }
    }

    let matches = detect_sensitive(content, config);
    if !matches.is_empty() {
        let labels: Vec<&str> = matches.iter().map(|m| m.kind.label()).collect();
        return Some(format!("Sensitive: {}", labels.join(", ")));
    }

    None
}

// ── Pattern Detectors ──

fn detect_api_keys(content: &str, matches: &mut Vec<SensitiveMatch>) {
    let prefixes: &[(&str, &str, usize)] = &[
        ("sk-", "OpenAI", 8),
        ("sk-proj-", "OpenAI Project", 8),
        ("AKIA", "AWS", 8),
        ("ghp_", "GitHub PAT", 8),
        ("gho_", "GitHub OAuth", 8),
        ("ghs_", "GitHub Server", 8),
        ("github_pat_", "GitHub Fine-grained", 8),
        ("glpat-", "GitLab", 8),
        ("xoxb-", "Slack Bot", 8),
        ("xoxp-", "Slack User", 8),
        ("xapp-", "Slack App", 8),
        ("SG.", "SendGrid", 8),
        ("sk_live_", "Stripe Live", 8),
        ("sk_test_", "Stripe Test", 8),
        ("rk_live_", "Stripe Restricted", 8),
        ("pk_live_", "Stripe Publishable", 8),
        ("whsec_", "Stripe Webhook", 8),
        ("npd_", "npm", 8),
        ("pypi-", "PyPI", 8),
        ("AIZA", "Google API", 8),
        ("hf_", "Hugging Face", 8),
    ];

    for (prefix, provider, min_suffix_len) in prefixes {
        for word in content.split_whitespace() {
            let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
            if trimmed.starts_with(prefix)
                && trimmed.len() >= prefix.len() + min_suffix_len
                && trimmed[prefix.len()..]
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                matches.push(SensitiveMatch {
                    kind: SensitiveKind::ApiKey(provider),
                    redacted_preview: format!("{}{}…", prefix, &"*".repeat(8)),
                });
                return;
            }
        }
    }

    if content.contains("-----BEGIN")
        && (content.contains("PRIVATE KEY") || content.contains("RSA PRIVATE"))
    {
        matches.push(SensitiveMatch {
            kind: SensitiveKind::PrivateKey,
            redacted_preview: "-----BEGIN ***PRIVATE KEY***-----".into(),
        });
    }

    for word in content.split_whitespace() {
        if word.starts_with("eyJ") && word.matches('.').count() == 2 && word.len() > 40 {
            matches.push(SensitiveMatch {
                kind: SensitiveKind::JwtToken,
                redacted_preview: "eyJ…[JWT]".into(),
            });
            return;
        }
    }
}

fn detect_credentials(content: &str, matches: &mut Vec<SensitiveMatch>) {
    let lower = content.to_lowercase();
    let secret_patterns = [
        "password=",
        "password:",
        "passwd=",
        "pwd=",
        "secret=",
        "secret_key=",
        "api_key=",
        "api_secret=",
        "access_token=",
        "auth_token=",
        "bearer ",
        "authorization: bearer",
        "database_url=",
        "connection_string=",
        "mongodb+srv://",
        "postgres://",
        "mysql://",
    ];

    for pat in &secret_patterns {
        if lower.contains(pat) {
            let has_value = content.to_lowercase().find(pat).and_then(|pos| {
                let after = &content[pos + pat.len()..];
                let value: String = after
                    .chars()
                    .take_while(|c| !c.is_whitespace() && *c != '\n')
                    .collect();
                if value.len() > 3 {
                    Some(value)
                } else {
                    None
                }
            });

            if has_value.is_some() {
                matches.push(SensitiveMatch {
                    kind: SensitiveKind::GenericSecret,
                    redacted_preview: format!("{}***", pat),
                });
                return;
            }
        }
    }
}

fn detect_credit_cards(content: &str, matches: &mut Vec<SensitiveMatch>) {
    let digits: String = content.chars().filter(|c| c.is_ascii_digit()).collect();

    if digits.len() < 13 || digits.len() > 19 {
        return;
    }

    let first = digits.chars().next().unwrap_or('0');
    if !['3', '4', '5', '6'].contains(&first) {
        return;
    }

    if luhn_check(&digits) {
        matches.push(SensitiveMatch {
            kind: SensitiveKind::CreditCard,
            redacted_preview: format!(
                "****-****-****-{}",
                &digits[digits.len().saturating_sub(4)..]
            ),
        });
    }
}

fn luhn_check(digits: &str) -> bool {
    let mut sum = 0u32;
    let mut double = false;

    for ch in digits.chars().rev() {
        let mut d = ch.to_digit(10).unwrap_or(0);
        if double {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
        double = !double;
    }

    sum % 10 == 0
}

fn detect_ssn(content: &str, matches: &mut Vec<SensitiveMatch>) {
    let chars: Vec<char> = content.chars().collect();
    if chars.len() < 11 {
        return;
    }

    for window in chars.windows(11) {
        if window[0].is_ascii_digit()
            && window[1].is_ascii_digit()
            && window[2].is_ascii_digit()
            && window[3] == '-'
            && window[4].is_ascii_digit()
            && window[5].is_ascii_digit()
            && window[6] == '-'
            && window[7].is_ascii_digit()
            && window[8].is_ascii_digit()
            && window[9].is_ascii_digit()
            && window[10].is_ascii_digit()
        {
            let area: u32 = window[0..3].iter().collect::<String>().parse().unwrap_or(0);
            if area > 0 && area != 666 && area < 900 {
                matches.push(SensitiveMatch {
                    kind: SensitiveKind::Ssn,
                    redacted_preview: "***-**-****".into(),
                });
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PrivacyConfig {
        PrivacyConfig::default()
    }

    #[test]
    fn test_api_key_detection() {
        let m = detect_sensitive("my key is sk-abcdefghijklmnopqrstuvwx", &cfg());
        assert!(!m.is_empty());
        assert!(matches!(m[0].kind, SensitiveKind::ApiKey("OpenAI")));
    }

    #[test]
    fn test_aws_key_detection() {
        let m = detect_sensitive("AKIAIOSFODNN7EXAMPLE", &cfg());
        assert!(!m.is_empty());
        assert!(matches!(m[0].kind, SensitiveKind::ApiKey("AWS")));
    }

    #[test]
    fn test_jwt_detection() {
        let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let m = detect_sensitive(token, &cfg());
        assert!(!m.is_empty());
        assert_eq!(m[0].kind, SensitiveKind::JwtToken);
    }

    #[test]
    fn test_credit_card_detection() {
        let m = detect_sensitive("4111 1111 1111 1111", &cfg());
        assert!(!m.is_empty());
        assert_eq!(m[0].kind, SensitiveKind::CreditCard);
    }

    #[test]
    fn test_ssn_detection() {
        let m = detect_sensitive("SSN: 123-45-6789", &cfg());
        assert!(!m.is_empty());
        assert_eq!(m[0].kind, SensitiveKind::Ssn);
    }

    #[test]
    fn test_password_detection() {
        let m = detect_sensitive("password=super_secret_123", &cfg());
        assert!(!m.is_empty());
        assert_eq!(m[0].kind, SensitiveKind::GenericSecret);
    }

    #[test]
    fn test_excluded_app() {
        let cfg = PrivacyConfig::default();
        assert!(is_excluded_app("1Password 7", &cfg));
        assert!(is_excluded_app("Bitwarden", &cfg));
        assert!(!is_excluded_app("Chrome", &cfg));
    }

    #[test]
    fn test_normal_text_not_flagged() {
        let m = detect_sensitive(
            "Hello, this is a normal sentence about programming.",
            &cfg(),
        );
        assert!(m.is_empty());
    }

    #[test]
    fn test_private_key_detection() {
        let m = detect_sensitive(
            "-----BEGIN RSA PRIVATE KEY-----\nMIIE...\n-----END RSA PRIVATE KEY-----",
            &cfg(),
        );
        assert!(!m.is_empty());
        assert_eq!(m[0].kind, SensitiveKind::PrivateKey);
    }
}
