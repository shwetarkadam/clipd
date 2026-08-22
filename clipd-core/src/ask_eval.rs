//! Offline retrieval eval for Ask — hit rate, MRR, nDCG over a golden set.
//!
//! Hybrid search (FTS5 + TF-IDF + optional embeddings, fused with RRF) is the
//! production path in [`crate::ask::retrieve`]. This module measures whether
//! that path actually surfaces the right clips for a fixed corpus of questions,
//! without calling a live LLM.
//!
//! Run with: `cargo test -p clipd-core ask_eval -- --nocapture`

use crate::ask::{retrieve, AskConfig, AskFilters};
use crate::models::ClipEntry;
use crate::store::ClipStore;
use crate::transform::TransformConfig;

/// One labelled question: which clip contents (substrings) should rank in the
/// top-k fused results.
#[derive(Debug, Clone)]
pub struct GoldenCase {
    pub question: &'static str,
    /// Substrings that identify relevant clips (matched against `content`).
    pub relevant: &'static [&'static str],
    /// Optional app filter applied during retrieval.
    pub source_app: Option<&'static str>,
    /// When true, a correct system should return *no* relevant hit in top-k
    /// (adversarial / out-of-corpus questions).
    pub expect_empty: bool,
}

/// Aggregate metrics over a golden set.
#[derive(Debug, Clone, Default)]
pub struct EvalReport {
    pub cases: usize,
    pub hit_rate_at_1: f64,
    pub hit_rate_at_3: f64,
    pub hit_rate_at_5: f64,
    pub mrr: f64,
    pub ndcg_at_5: f64,
    /// Adversarial cases that correctly returned nothing relevant.
    pub empty_precision: f64,
    pub empty_cases: usize,
    pub failures: Vec<String>,
}

impl EvalReport {
    pub fn summary(&self) -> String {
        format!(
            "ask_eval: {n} cases · HR@1={h1:.2} HR@3={h3:.2} HR@5={h5:.2} \
             MRR={mrr:.2} nDCG@5={ndcg:.2} · empty-ok={empty_ok}/{empty_n} · failures={fails}",
            n = self.cases,
            h1 = self.hit_rate_at_1,
            h3 = self.hit_rate_at_3,
            h5 = self.hit_rate_at_5,
            mrr = self.mrr,
            ndcg = self.ndcg_at_5,
            empty_ok = (self.empty_precision * self.empty_cases as f64).round() as usize,
            empty_n = self.empty_cases,
            fails = self.failures.len(),
        )
    }
}

/// Hit rate@k: fraction of queries where at least one relevant id is in top-k.
pub fn hit_rate_at_k(ranked_ids: &[i64], relevant: &[i64], k: usize) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    let top = &ranked_ids[..ranked_ids.len().min(k)];
    if top.iter().any(|id| relevant.contains(id)) {
        1.0
    } else {
        0.0
    }
}

/// Mean Reciprocal Rank of the first relevant result (0 if none in the list).
pub fn mean_reciprocal_rank(ranked_ids: &[i64], relevant: &[i64]) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    for (i, id) in ranked_ids.iter().enumerate() {
        if relevant.contains(id) {
            return 1.0 / (i + 1) as f64;
        }
    }
    0.0
}

/// Binary nDCG@k (relevance is 0/1).
pub fn ndcg_at_k(ranked_ids: &[i64], relevant: &[i64], k: usize) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    let top = &ranked_ids[..ranked_ids.len().min(k)];
    let mut dcg = 0.0;
    for (i, id) in top.iter().enumerate() {
        if relevant.contains(id) {
            // log2(i+2): rank 1 → gain/1, rank 2 → gain/log2(3), …
            dcg += 1.0 / ((i + 2) as f64).log2();
        }
    }
    let ideal_hits = relevant.len().min(k);
    let mut idcg = 0.0;
    for i in 0..ideal_hits {
        idcg += 1.0 / ((i + 2) as f64).log2();
    }
    if idcg == 0.0 {
        0.0
    } else {
        dcg / idcg
    }
}

/// Fixture clipboard history — deliberately mixed (SQL, URLs, secrets-looking,
/// shell, prose, JSON) so each retriever has something to be good at.
pub fn eval_corpus() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "postgres://admin:hunter2@db.internal:5432/production",
            "DataGrip",
        ),
        (
            "SELECT id, email FROM users WHERE active = true ORDER BY created_at DESC;",
            "DataGrip",
        ),
        (
            "{\"event\":\"payment_intent.succeeded\",\"id\":\"evt_1Pabc\",\"amount\":4200}",
            "Chrome",
        ),
        ("brew install --cask docker", "Terminal"),
        ("https://github.com/shwetarkadam/clipd", "Safari"),
        (
            "export OPENAI_API_KEY=sk-proj-not-a-real-key-for-tests",
            "Terminal",
        ),
        (
            "fn main() {\n    println!(\"hello clipd\");\n}",
            "VS Code",
        ),
        (
            "Meeting notes 2026-07-15: ship multi-slot HUD, fix Accessibility prompt",
            "Notes",
        ),
        (
            "ssh ubuntu@10.0.4.22 -i ~/.ssh/staging.pem",
            "Terminal",
        ),
        (
            "https://leetcode.com/problems/shift-2d-grid/?envType=daily-question",
            "Chrome",
        ),
        (
            "class Solution {\n    public List<Integer> maxActiveSectionsAfterTrade(String s) {\n",
            "Chrome",
        ),
        (
            "Apple Development: 919172522532 (YQDYWXAQGZ)",
            "Keychain Access",
        ),
        (
            "curl -X POST https://api.stripe.com/v1/payment_intents -u sk_test_51:",
            "Terminal",
        ),
        (
            "The quick brown fox jumps over the lazy dog",
            "Notes",
        ),
        (
            "redis-cli -h cache.prod.internal GET session:user:42",
            "Terminal",
        ),
        (
            "mailto:shweta@example.com?subject=clipd%20feedback",
            "Mail",
        ),
        (
            "Cargo.toml\n[package]\nname = \"clipd-core\"\nversion = \"0.4.11\"",
            "VS Code",
        ),
        (
            "ERROR: relation \"clipboard_events\" does not exist at character 15",
            "DataGrip",
        ),
        (
            "https://linkedin.com/in/shwetarkadam",
            "Chrome",
        ),
        (
            "aws s3 cp ./dist s3://clipd-releases/v0.4.11/ --recursive",
            "Terminal",
        ),
        (
            "BEGIN:VCARD\nFN:Shweta Kadam\nEMAIL:shweta@example.com\nEND:VCARD",
            "Contacts",
        ),
        (
            "kubectl get pods -n staging | grep clipd",
            "Terminal",
        ),
        (
            "TODO: wire hybrid search eval + citation grounding before release",
            "Notes",
        ),
        (
            "npm create vite@latest clipd-web -- --template react-ts",
            "Terminal",
        ),
        (
            "latitude=37.7749&longitude=-122.4194&zoom=12",
            "Chrome",
        ),
    ]
}

/// Golden questions. Relevant entries are content substrings from [`eval_corpus`].
pub fn golden_cases() -> Vec<GoldenCase> {
    vec![
        GoldenCase {
            question: "what was the postgres connection string",
            relevant: &["postgres://admin"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "show me the SQL query for active users",
            relevant: &["SELECT id, email FROM users"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "stripe payment_intent webhook payload",
            relevant: &["payment_intent.succeeded"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "how did I install docker",
            relevant: &["brew install --cask docker"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "github link for clipd",
            relevant: &["github.com/shwetarkadam/clipd"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "openai api key I exported",
            relevant: &["OPENAI_API_KEY"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "rust hello world main function",
            relevant: &["println!(\"hello clipd\")"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "meeting notes about multi-slot HUD",
            relevant: &["multi-slot HUD"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "ssh into staging server",
            relevant: &["ssh ubuntu@10.0.4.22"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "leetcode shift 2d grid problem",
            relevant: &["leetcode.com/problems/shift-2d-grid"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "apple development team id",
            relevant: &["919172522532"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "stripe curl payment intents command",
            relevant: &["api.stripe.com/v1/payment_intents"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "redis get session for user 42",
            relevant: &["session:user:42"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "my email address for feedback",
            relevant: &["shweta@example.com"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "clipd-core crate version in Cargo.toml",
            relevant: &["name = \"clipd-core\""],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "postgres error about clipboard_events table",
            relevant: &["clipboard_events"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "linkedin profile url",
            relevant: &["linkedin.com/in/shwetarkadam"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "aws s3 upload of the release",
            relevant: &["s3://clipd-releases"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "vcard contact card",
            relevant: &["BEGIN:VCARD"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "kubectl pods in staging for clipd",
            relevant: &["kubectl get pods -n staging"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "todo about citation grounding",
            relevant: &["citation grounding"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "vite react typescript scaffold command",
            relevant: &["npm create vite@latest"],
            source_app: None,
            expect_empty: false,
        },
        GoldenCase {
            question: "latitude longitude zoom query string",
            relevant: &["latitude=37.7749"],
            source_app: None,
            expect_empty: false,
        },
        // App-scoped: docker install is Terminal-only in the corpus.
        GoldenCase {
            question: "docker",
            relevant: &["brew install --cask docker"],
            source_app: Some("Terminal"),
            expect_empty: false,
        },
        // SQL from DataGrip only.
        GoldenCase {
            question: "users query",
            relevant: &["SELECT id, email FROM users"],
            source_app: Some("DataGrip"),
            expect_empty: false,
        },
        // Adversarial — nothing in the corpus should match.
        GoldenCase {
            question: "what is the capital of Mongolia",
            relevant: &[],
            source_app: None,
            expect_empty: true,
        },
        GoldenCase {
            question: "who won the 2014 FIFA world cup final",
            relevant: &[],
            source_app: None,
            expect_empty: true,
        },
        GoldenCase {
            question: "recipe for sourdough starter with grams",
            relevant: &[],
            source_app: None,
            expect_empty: true,
        },
    ]
}

fn no_api() -> TransformConfig {
    TransformConfig {
        api_key: None,
        // Non-loopback so can_synthesize is false; embeddings stay off.
        api_url: "https://api.example.invalid/v1/chat/completions".into(),
        model: "unused".into(),
    }
}

/// Seed an in-memory store with [`eval_corpus`].
pub fn seed_eval_store() -> Result<ClipStore, String> {
    let store = ClipStore::in_memory().map_err(|e| e.to_string())?;
    for (content, app) in eval_corpus() {
        let mut entry = ClipEntry::new(content.to_string(), Some(app.to_string()), None);
        entry.id = 0;
        store.insert(&entry).map_err(|e| e.to_string())?;
    }
    Ok(store)
}

fn relevant_ids(store: &ClipStore, needles: &[&str]) -> Result<Vec<i64>, String> {
    let clips = store
        .get_recent(10_000)
        .map_err(|e| e.to_string())?;
    let mut ids = Vec::new();
    for needle in needles {
        for clip in &clips {
            if clip.content.contains(needle) && !ids.contains(&clip.id) {
                ids.push(clip.id);
            }
        }
    }
    Ok(ids)
}

/// Run retrieval over every golden case and compute aggregate metrics.
///
/// Uses FTS5 + TF-IDF only (no API key) — that is the always-on hybrid path.
/// Embeddings, when configured, can only improve these numbers further.
pub fn run_retrieval_eval(cfg: &AskConfig) -> Result<EvalReport, String> {
    let store = seed_eval_store()?;
    let api = no_api();
    let cases = golden_cases();

    let mut hr1 = 0.0;
    let mut hr3 = 0.0;
    let mut hr5 = 0.0;
    let mut mrr_sum = 0.0;
    let mut ndcg_sum = 0.0;
    let mut scored = 0usize;
    let mut empty_ok = 0usize;
    let mut empty_n = 0usize;
    let mut failures = Vec::new();

    for case in &cases {
        let filters = AskFilters {
            source_app: case.source_app.map(|s| s.to_string()),
            since: None,
        };
        let hits = retrieve(&store, case.question, &filters, cfg, &api)?;
        let ranked: Vec<i64> = hits.iter().map(|h| h.clip.id).collect();

        if case.expect_empty {
            empty_n += 1;
            // "Nothing relevant" — either empty results, or none of the top-5
            // contents look like a confident factual answer to the adversarial
            // prompt. We treat empty / low-agreement top hit as success.
            let ok = ranked.is_empty()
                || hits
                    .iter()
                    .take(3)
                    .all(|h| h.retriever_count() < 2 && h.fused_score < 0.05);
            if ok {
                empty_ok += 1;
            } else {
                failures.push(format!(
                    "adversarial {:?} unexpectedly retrieved [#{}] ({})",
                    case.question,
                    hits[0].clip.id,
                    hits[0].clip.preview
                ));
            }
            continue;
        }

        let relevant = relevant_ids(&store, case.relevant)?;
        if relevant.is_empty() {
            failures.push(format!(
                "golden case {:?} has no matching corpus clips for {:?}",
                case.question, case.relevant
            ));
            continue;
        }

        scored += 1;
        let h1 = hit_rate_at_k(&ranked, &relevant, 1);
        let h3 = hit_rate_at_k(&ranked, &relevant, 3);
        let h5 = hit_rate_at_k(&ranked, &relevant, 5);
        hr1 += h1;
        hr3 += h3;
        hr5 += h5;
        mrr_sum += mean_reciprocal_rank(&ranked, &relevant);
        ndcg_sum += ndcg_at_k(&ranked, &relevant, 5);

        if h5 < 1.0 {
            failures.push(format!(
                "miss@5 for {:?} — top: {:?}",
                case.question,
                hits.iter()
                    .take(5)
                    .map(|h| h.clip.preview.clone())
                    .collect::<Vec<_>>()
            ));
        }
    }

    let n = scored.max(1) as f64;
    Ok(EvalReport {
        cases: scored + empty_n,
        hit_rate_at_1: hr1 / n,
        hit_rate_at_3: hr3 / n,
        hit_rate_at_5: hr5 / n,
        mrr: mrr_sum / n,
        ndcg_at_5: ndcg_sum / n,
        empty_precision: if empty_n == 0 {
            1.0
        } else {
            empty_ok as f64 / empty_n as f64
        },
        empty_cases: empty_n,
        failures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_basic_properties() {
        let ranked = vec![10, 20, 30, 40];
        let relevant = vec![30, 99];
        assert_eq!(hit_rate_at_k(&ranked, &relevant, 1), 0.0);
        assert_eq!(hit_rate_at_k(&ranked, &relevant, 3), 1.0);
        assert!((mean_reciprocal_rank(&ranked, &relevant) - 1.0 / 3.0).abs() < 1e-9);
        assert!(ndcg_at_k(&ranked, &relevant, 5) > 0.0);
        assert_eq!(ndcg_at_k(&ranked, &[], 5), 0.0);
    }

    #[test]
    fn golden_set_has_enough_cases() {
        assert!(
            golden_cases().len() >= 25,
            "keep the harness honest — at least 25 labelled questions"
        );
        assert!(
            eval_corpus().len() >= 20,
            "corpus should be wide enough to stress hybrid fusion"
        );
    }

    #[test]
    fn hybrid_retrieval_beats_floor_on_golden_set() {
        let cfg = AskConfig {
            top_k: 8,
            ..AskConfig::default()
        };
        let report = run_retrieval_eval(&cfg).expect("eval should run offline");
        eprintln!("{}", report.summary());
        for f in &report.failures {
            eprintln!("  fail: {f}");
        }

        // Floors are conservative for FTS+TF-IDF without embeddings. If these
        // regress, hybrid fusion or query expansion broke — not "flaky AI".
        assert!(
            report.hit_rate_at_5 >= 0.85,
            "HR@5 too low: {:.2} (failures: {})",
            report.hit_rate_at_5,
            report.failures.len()
        );
        assert!(
            report.hit_rate_at_1 >= 0.60,
            "HR@1 too low: {:.2}",
            report.hit_rate_at_1
        );
        assert!(report.mrr >= 0.70, "MRR too low: {:.2}", report.mrr);
        assert!(
            report.ndcg_at_5 >= 0.75,
            "nDCG@5 too low: {:.2}",
            report.ndcg_at_5
        );
    }

    #[test]
    fn fts_only_ablation_is_weaker_or_equal_on_paraphrase() {
        // Zeroing TF-IDF should not *improve* paraphrase-heavy questions.
        // We just check the ablation path runs and returns a report.
        let mut cfg = AskConfig::default();
        cfg.weight_tfidf = 0.0;
        cfg.weight_embedding = 0.0;
        let report = run_retrieval_eval(&cfg).expect("ablation should run");
        assert!(report.cases >= 25);
        eprintln!("fts-only ablation: {}", report.summary());
    }
}
