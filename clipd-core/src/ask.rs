//! Ask — grounded question answering over clipboard history (RAG).
//!
//! Three retrievers already exist in this crate and each is good at something
//! different: FTS5 (`ClipStore::search`) nails exact tokens and rare strings,
//! TF-IDF (`semantic::TfIdfIndex`) survives paraphrase, and cloud embeddings
//! (`embedding::search_embeddings`) catch true synonymy. `retrieve` runs all
//! three and fuses their *rankings* — not their scores, which are on wildly
//! different scales — with weighted Reciprocal Rank Fusion.
//!
//! The generation half is deliberately paranoid. Clips are the user's real
//! keystrokes, so before anything leaves the machine every candidate goes
//! through `privacy::detect_sensitive` and secrets are dropped. The model is
//! told to answer only from the numbered clips it is shown and to cite
//! `[#id]`; on the way back every citation is checked against the context that
//! was actually sent, and ids the model invented are stripped rather than
//! rendered. Grounding you can't verify isn't grounding.
//!
//! With no API key configured the whole generation step is skipped and the
//! fused ranking is returned as-is (`AskAnswer::retrieval_only`). Recall still
//! works, entirely locally — that's the Tier-1 promise.

use crate::embedding::{generate_embedding, is_embedding_available, search_embeddings};
use crate::models::{ClipEntry, SearchFilters};
use crate::privacy::{detect_sensitive, load_privacy_config};
use crate::semantic::TfIdfIndex;
use crate::store::ClipStore;
use crate::transform::TransformConfig;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Config ──

/// Tunables for one ask. `Default` is the shipping configuration; the CLI and
/// GUI only override `top_k`/filters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskConfig {
    /// How many candidates each retriever contributes to the fusion pool.
    pub candidates_per_retriever: usize,
    /// How many fused clips actually make it into the prompt.
    pub top_k: usize,
    /// RRF damping constant. 60 is the value from the original RRF paper and
    /// is deliberately large: it flattens the curve so a clip ranked #1 by one
    /// retriever doesn't automatically beat a clip ranked #3 by all three.
    pub rrf_k: f64,
    /// Per-retriever weight applied to its RRF contribution.
    pub weight_fulltext: f64,
    pub weight_tfidf: f64,
    pub weight_embedding: f64,
    /// Character budget for the whole assembled context.
    pub max_context_chars: usize,
    /// Character budget for any single clip inside the context.
    pub max_clip_chars: usize,
    /// How many recent history clips TF-IDF indexes in memory.
    pub tfidf_pool: usize,
    /// Low for recall: this task is "read these clips and report", not compose.
    /// f64 so it serializes as a clean `0.1` rather than an f32 artefact.
    pub temperature: f64,
    pub max_tokens: u32,
    /// How many prior turns of the thread are replayed to the model.
    pub history_turns: usize,
    /// Drop clips containing detected secrets instead of sending them upstream.
    pub redact_secrets: bool,
}

impl Default for AskConfig {
    fn default() -> Self {
        Self {
            candidates_per_retriever: 25,
            top_k: 8,
            rrf_k: 60.0,
            weight_fulltext: 1.0,
            weight_tfidf: 0.9,
            // Embeddings earn a slight edge when present: they're the only
            // retriever that can match on meaning alone, which is exactly the
            // case the other two miss.
            weight_embedding: 1.2,
            max_context_chars: 12_000,
            max_clip_chars: 1_500,
            tfidf_pool: 600,
            temperature: 0.1,
            max_tokens: 1024,
            history_turns: 4,
            redact_secrets: true,
        }
    }
}

/// Optional narrowing applied before retrieval (mirrors `clipd search` flags).
#[derive(Debug, Clone, Default)]
pub struct AskFilters {
    pub source_app: Option<String>,
    pub since: Option<DateTime<Utc>>,
}

// ── Retrieval ──

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Retriever {
    FullText,
    TfIdf,
    Embedding,
}

impl Retriever {
    pub fn label(&self) -> &'static str {
        match self {
            Self::FullText => "full-text",
            Self::TfIdf => "tf-idf",
            Self::Embedding => "embedding",
        }
    }
}

/// A clip that survived fusion, with the provenance of *why* it survived.
#[derive(Debug, Clone)]
pub struct RetrievedClip {
    pub clip: ClipEntry,
    /// Weighted RRF score. Comparable within one ask, meaningless across asks.
    pub fused_score: f64,
    /// Which retrievers found it, and at what 1-based rank.
    pub hits: Vec<(Retriever, usize)>,
    /// Set when the clip was withheld from the model for containing secrets.
    pub withheld: Option<String>,
}

impl RetrievedClip {
    /// Agreement across independent retrievers is the single best signal we
    /// have that a hit is real rather than a lexical coincidence.
    pub fn retriever_count(&self) -> usize {
        self.hits.len()
    }

    pub fn matched_by(&self) -> String {
        self.hits
            .iter()
            .map(|(r, _)| r.label())
            .collect::<Vec<_>>()
            .join(" + ")
    }
}

// ── Answer ──

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    /// Cited clips that more than one retriever independently surfaced.
    High,
    /// Cited real clips, but from a single retriever's ranking.
    Medium,
    /// Answered without citing anything checkable.
    Low,
    /// Nothing relevant found, or the model declined to answer.
    None,
}

impl Confidence {
    pub fn label(&self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::None => "none",
        }
    }
}

/// One clip the answer actually cited, resolved back to a real row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskSource {
    pub clip_id: i64,
    pub preview: String,
    pub source_app: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub matched_by: String,
    pub fused_score: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct AskAnswer {
    pub question: String,
    /// Empty in retrieval-only mode — read `retrieved` instead.
    pub answer: String,
    /// Citations that resolved to clips genuinely present in the context.
    pub sources: Vec<AskSource>,
    /// Everything fusion considered, best first, including withheld clips.
    pub retrieved: Vec<RetrievedClip>,
    pub confidence: Confidence,
    /// True when no API key was configured and generation was skipped.
    pub retrieval_only: bool,
    /// Citations the model emitted for clips that were never in its context.
    /// Non-empty means the model tried to fabricate a source.
    pub invalid_citations: Vec<i64>,
    /// Clips withheld from the prompt because they contained secrets.
    pub withheld_count: usize,
    pub usage: Option<Usage>,
    pub estimated_prompt_tokens: usize,
}

impl AskAnswer {
    /// Plain-text rendering shared by the CLI and the MCP tool.
    pub fn render(&self) -> String {
        let mut out = String::new();

        if self.retrieval_only {
            out.push_str(
                "No API key configured — showing what clipd found, without a written answer.\n\
                 (Set api_key in transform.json to get synthesized answers.)\n\n",
            );
            if self.retrieved.is_empty() {
                out.push_str("No matching clips.\n");
                return out;
            }
            for (i, r) in self.retrieved.iter().enumerate() {
                out.push_str(&format!(
                    "{}. [#{}] {}  ({}, score {:.4})\n",
                    i + 1,
                    r.clip.id,
                    one_line(&r.clip.preview, 88),
                    r.matched_by(),
                    r.fused_score
                ));
            }
            return out;
        }

        out.push_str(&self.answer);
        out.push_str("\n\n");

        if self.sources.is_empty() {
            out.push_str("Sources: none cited\n");
        } else {
            out.push_str("Sources:\n");
            for s in &self.sources {
                let app = s.source_app.as_deref().unwrap_or("unknown app");
                out.push_str(&format!(
                    "  [#{}] {}  — {}, {}  ({})\n",
                    s.clip_id,
                    one_line(&s.preview, 72),
                    app,
                    s.timestamp.format("%Y-%m-%d %H:%M"),
                    s.matched_by
                ));
            }
        }

        out.push_str(&format!(
            "Confidence: {} · {} clips retrieved, {} cited",
            self.confidence.label(),
            self.retrieved.len(),
            self.sources.len()
        ));
        if self.withheld_count > 0 {
            out.push_str(&format!(
                " · {} withheld (secrets)",
                self.withheld_count
            ));
        }
        if let Some(u) = &self.usage {
            out.push_str(&format!(" · {} tokens", u.total_tokens));
        }
        out.push('\n');

        if !self.invalid_citations.is_empty() {
            out.push_str(&format!(
                "Warning: the model cited {} clip id(s) that were not in its context; \
                 those citations were dropped.\n",
                self.invalid_citations.len()
            ));
        }
        out
    }
}

// ── Threads ──

/// One question/answer exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskTurn {
    pub question: String,
    pub answer: String,
    #[serde(default)]
    pub cited_ids: Vec<i64>,
    pub asked_at: DateTime<Utc>,
}

/// A conversation. Lives in memory for the session; when `id` is set it is
/// also written through to SQLite, so a follow-up survives a restart.
#[derive(Debug, Clone, Default)]
pub struct AskThread {
    pub id: Option<i64>,
    pub turns: Vec<AskTurn>,
}

impl AskThread {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach to a persisted thread, hydrating the in-memory turns from disk.
    pub fn load(store: &ClipStore, thread_id: i64) -> Self {
        let turns = store.ask_thread_turns(thread_id).unwrap_or_default();
        Self {
            id: Some(thread_id),
            turns,
        }
    }

    /// Resume the most recent persisted thread, or start a fresh one.
    pub fn resume_latest(store: &ClipStore) -> Self {
        match store.latest_ask_thread() {
            Ok(Some(id)) => Self::load(store, id),
            _ => Self::new(),
        }
    }

    /// Record a turn in memory and, best-effort, on disk. Persistence failure
    /// must never lose the answer the user is currently reading, so errors
    /// here are logged and swallowed.
    pub fn record(&mut self, store: &ClipStore, answer: &AskAnswer) {
        let turn = AskTurn {
            question: answer.question.clone(),
            answer: answer.answer.clone(),
            cited_ids: answer.sources.iter().map(|s| s.clip_id).collect(),
            asked_at: Utc::now(),
        };

        if self.id.is_none() {
            match store.create_ask_thread(&title_from(&answer.question)) {
                Ok(id) => self.id = Some(id),
                Err(e) => log::debug!("ask: could not create thread: {}", e),
            }
        }
        if let Some(id) = self.id {
            if let Err(e) = store.append_ask_turn(id, &turn) {
                log::debug!("ask: could not persist turn: {}", e);
            }
        }

        self.turns.push(turn);
    }
}

fn title_from(question: &str) -> String {
    one_line(question, 60)
}

// ── Entry point ──

/// Whether synthesis is possible at all. When false, `ask` returns the fused
/// ranking and nothing leaves the machine.
pub fn has_api_key(api: &TransformConfig) -> bool {
    api.api_key.as_deref().map_or(false, |k| !k.is_empty())
}

/// Retrieve, then (when a key is configured) answer.
pub fn ask(
    store: &ClipStore,
    question: &str,
    thread: &AskThread,
    filters: &AskFilters,
    cfg: &AskConfig,
    api: &TransformConfig,
) -> Result<AskAnswer, String> {
    let question = question.trim();
    if question.is_empty() {
        return Err("Ask what? Give me a question.".into());
    }

    // A follow-up like "and the one before that?" has almost no retrievable
    // content on its own. Fold the previous question in so retrieval has
    // something to work with; the model still sees the turns verbatim.
    let retrieval_query = expand_query(question, thread);
    let mut retrieved = retrieve(store, &retrieval_query, filters, cfg, api)?;

    if cfg.redact_secrets {
        mark_secrets(&mut retrieved);
    }
    let withheld_count = retrieved.iter().filter(|r| r.withheld.is_some()).count();

    if !has_api_key(api) {
        return Ok(AskAnswer {
            question: question.to_string(),
            answer: String::new(),
            sources: Vec::new(),
            retrieved,
            confidence: Confidence::None,
            retrieval_only: true,
            invalid_citations: Vec::new(),
            withheld_count,
            usage: None,
            estimated_prompt_tokens: 0,
        });
    }

    let usable: Vec<&RetrievedClip> = retrieved.iter().filter(|r| r.withheld.is_none()).collect();

    if usable.is_empty() {
        return Ok(AskAnswer {
            question: question.to_string(),
            answer: "I couldn't find anything in your clipboard history about that.".into(),
            sources: Vec::new(),
            retrieved,
            confidence: Confidence::None,
            retrieval_only: false,
            invalid_citations: Vec::new(),
            withheld_count,
            usage: None,
            estimated_prompt_tokens: 0,
        });
    }

    let context = build_context(&usable, cfg);
    let estimated_prompt_tokens = estimate_tokens(&context);
    let (raw, usage) = generate(question, &context, thread, cfg, api)?;

    // Only ids we actually put in front of the model count as citable.
    let allowed: Vec<i64> = usable.iter().map(|r| r.clip.id).collect();
    let (answer, cited, invalid_citations) = resolve_citations(&raw, &allowed);

    let sources: Vec<AskSource> = cited
        .iter()
        .filter_map(|id| retrieved.iter().find(|r| r.clip.id == *id))
        .map(|r| AskSource {
            clip_id: r.clip.id,
            preview: r.clip.preview.clone(),
            source_app: r.clip.source_app.clone(),
            timestamp: r.clip.timestamp,
            matched_by: r.matched_by(),
            fused_score: r.fused_score,
        })
        .collect();

    let confidence = score_confidence(&answer, &sources, &retrieved);

    Ok(AskAnswer {
        question: question.to_string(),
        answer,
        sources,
        retrieved,
        confidence,
        retrieval_only: false,
        invalid_citations,
        withheld_count,
        usage,
        estimated_prompt_tokens,
    })
}

/// Hybrid retrieval with weighted Reciprocal Rank Fusion.
pub fn retrieve(
    store: &ClipStore,
    query: &str,
    filters: &AskFilters,
    cfg: &AskConfig,
    api: &TransformConfig,
) -> Result<Vec<RetrievedClip>, String> {
    let mut pool: HashMap<i64, ClipEntry> = HashMap::new();
    let mut ranked: Vec<(Retriever, Vec<i64>)> = Vec::new();

    // 1. FTS5 — exact tokens, rare identifiers, error strings.
    let fts_hits = fulltext_candidates(store, query, filters, cfg);
    let mut fts_ids = Vec::with_capacity(fts_hits.len());
    for clip in fts_hits {
        fts_ids.push(clip.id);
        pool.entry(clip.id).or_insert(clip);
    }
    if !fts_ids.is_empty() {
        ranked.push((Retriever::FullText, fts_ids));
    }

    // 2. TF-IDF over recent history — paraphrase-tolerant, fully local.
    let recent = store
        .search(&SearchFilters {
            query: None,
            content_type: None,
            source_app: filters.source_app.clone(),
            since: filters.since,
            limit: cfg.tfidf_pool,
        })
        .map_err(|e| format!("History read failed: {}", e))?;

    if !recent.is_empty() {
        let docs: Vec<String> = recent.iter().map(searchable_text).collect();
        let refs: Vec<&str> = docs.iter().map(|s| s.as_str()).collect();
        let index = TfIdfIndex::build(&refs);
        let tfidf_ids: Vec<i64> = index
            .search(query, cfg.candidates_per_retriever)
            .into_iter()
            .filter_map(|r| recent.get(r.clip_index))
            .map(|clip| {
                pool.entry(clip.id).or_insert_with(|| clip.clone());
                clip.id
            })
            .collect();
        if !tfidf_ids.is_empty() {
            ranked.push((Retriever::TfIdf, tfidf_ids));
        }
    }

    // 3. Embeddings — optional, and only over clips already embedded.
    if let Some(emb_ids) = embedding_candidates(store, query, cfg, api, &mut pool) {
        if !emb_ids.is_empty() {
            ranked.push((Retriever::Embedding, emb_ids));
        }
    }

    Ok(fuse(ranked, pool, cfg))
}

/// Weighted RRF: score(d) = Σ_r w_r / (k + rank_r(d)).
///
/// Fusing ranks rather than scores is the whole point — FTS5 rank, TF-IDF
/// cosine, and embedding cosine are not commensurable, and normalizing them
/// against each other would be inventing a comparison that doesn't exist.
fn fuse(
    ranked: Vec<(Retriever, Vec<i64>)>,
    mut pool: HashMap<i64, ClipEntry>,
    cfg: &AskConfig,
) -> Vec<RetrievedClip> {
    let mut scores: HashMap<i64, f64> = HashMap::new();
    let mut hits: HashMap<i64, Vec<(Retriever, usize)>> = HashMap::new();

    for (retriever, ids) in ranked {
        let weight = match retriever {
            Retriever::FullText => cfg.weight_fulltext,
            Retriever::TfIdf => cfg.weight_tfidf,
            Retriever::Embedding => cfg.weight_embedding,
        };
        for (i, id) in ids.iter().enumerate() {
            let rank = i + 1;
            *scores.entry(*id).or_insert(0.0) += weight / (cfg.rrf_k + rank as f64);
            hits.entry(*id).or_default().push((retriever, rank));
        }
    }

    let mut out: Vec<RetrievedClip> = scores
        .into_iter()
        .filter_map(|(id, score)| {
            pool.remove(&id).map(|clip| RetrievedClip {
                clip,
                fused_score: score,
                hits: hits.remove(&id).unwrap_or_default(),
                withheld: None,
            })
        })
        .collect();

    out.sort_by(|a, b| {
        b.fused_score
            .partial_cmp(&a.fused_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            // Ties broken by recency: for a clipboard, newer is usually what
            // the user meant.
            .then_with(|| b.clip.timestamp.cmp(&a.clip.timestamp))
    });
    out.truncate(cfg.top_k);
    out
}

fn fulltext_candidates(
    store: &ClipStore,
    query: &str,
    filters: &AskFilters,
    cfg: &AskConfig,
) -> Vec<ClipEntry> {
    // FTS5 gets the phrase first, then each significant term. A natural
    // language question rarely matches as a phrase, so the per-term pass is
    // what usually carries this retriever.
    let mut seen: Vec<ClipEntry> = Vec::new();
    let mut ids: Vec<i64> = Vec::new();

    let mut queries: Vec<String> = vec![query.to_string()];
    queries.extend(
        significant_terms(query)
            .into_iter()
            .take(6)
            .map(String::from),
    );

    for q in queries {
        if seen.len() >= cfg.candidates_per_retriever {
            break;
        }
        let hits = store.search(&SearchFilters {
            query: Some(q),
            content_type: None,
            source_app: filters.source_app.clone(),
            since: filters.since,
            limit: cfg.candidates_per_retriever,
        });
        if let Ok(hits) = hits {
            for clip in hits {
                if !ids.contains(&clip.id) {
                    ids.push(clip.id);
                    seen.push(clip);
                }
            }
        }
    }

    seen.truncate(cfg.candidates_per_retriever);
    seen
}

fn embedding_candidates(
    store: &ClipStore,
    query: &str,
    cfg: &AskConfig,
    api: &TransformConfig,
    pool: &mut HashMap<i64, ClipEntry>,
) -> Option<Vec<i64>> {
    // Must use the caller's config, not the one on disk: `--no-ai` and the
    // MCP `retrieval_only` flag work by blanking the key, and re-reading the
    // file here would fire an embeddings request they explicitly opted out of.
    if !is_embedding_available(api) {
        return None;
    }
    if store.embedding_count().unwrap_or(0) == 0 {
        return None;
    }

    let query_vec = match generate_embedding(query, api) {
        Ok(v) => v,
        Err(e) => {
            // A dead embedding endpoint must not take the whole ask down —
            // the other two retrievers are still perfectly good.
            log::debug!("ask: embedding retriever unavailable: {}", e);
            return None;
        }
    };

    let stored = store.get_all_embeddings().ok()?;
    let ids: Vec<i64> = search_embeddings(&query_vec, &stored, cfg.candidates_per_retriever, 0.15)
        .into_iter()
        .filter_map(|r| {
            if pool.contains_key(&r.clip_id) {
                Some(r.clip_id)
            } else {
                store.get_by_id(r.clip_id).ok().map(|clip| {
                    let id = clip.id;
                    pool.insert(id, clip);
                    id
                })
            }
        })
        .collect();

    Some(ids)
}

// ── Context assembly ──

/// Flag clips whose content trips the secret detectors. They stay in the
/// result list so the user can see *that* something matched, but their
/// content never reaches the API.
fn mark_secrets(retrieved: &mut [RetrievedClip]) {
    let privacy = load_privacy_config();
    for r in retrieved.iter_mut() {
        let found = detect_sensitive(&r.clip.content, &privacy);
        if let Some(first) = found.first() {
            r.withheld = Some(first.kind.label().to_string());
        }
    }
}

/// Pack the highest-ranked clips into a numbered, budgeted context block.
fn build_context(clips: &[&RetrievedClip], cfg: &AskConfig) -> String {
    let mut out = String::new();
    let mut used = 0usize;

    for r in clips {
        let body = truncate_chars(&clip_body(&r.clip), cfg.max_clip_chars);
        let app = r.clip.source_app.as_deref().unwrap_or("unknown app");
        let title = r
            .clip
            .source_title
            .as_deref()
            .map(|t| format!(" — {}", one_line(t, 60)))
            .unwrap_or_default();

        let block = format!(
            "[#{}] {} · {}{} · matched by {}\n{}\n\n",
            r.clip.id,
            r.clip.timestamp.format("%Y-%m-%d %H:%M"),
            app,
            title,
            r.matched_by(),
            body
        );

        if used + block.len() > cfg.max_context_chars && !out.is_empty() {
            break;
        }
        used += block.len();
        out.push_str(&block);
    }

    out
}

/// Image clips carry their meaning in OCR text, not `content`.
fn clip_body(clip: &ClipEntry) -> String {
    match clip.ocr_text.as_deref() {
        Some(ocr) if !ocr.trim().is_empty() && clip.content.trim().is_empty() => ocr.to_string(),
        _ => clip.content.clone(),
    }
}

/// What TF-IDF indexes: content plus provenance, so "the SQL from DataGrip"
/// can match on the window title as well as the body.
fn searchable_text(clip: &ClipEntry) -> String {
    let mut s = clip_body(clip);
    if let Some(app) = &clip.source_app {
        s.push(' ');
        s.push_str(app);
    }
    if let Some(title) = &clip.source_title {
        s.push(' ');
        s.push_str(title);
    }
    s
}

// ── Generation ──

const SYSTEM_PROMPT: &str = "\
You answer questions about the user's clipboard history. You are shown numbered \
clips; each begins with a bracketed id like [#42].

Rules, in order of importance:
1. Answer ONLY from the clips provided. You have no other knowledge of this \
user, their files, or their history.
2. Cite the id of every clip you use, inline, in the form [#42]. An answer with \
no citation is only acceptable when you found nothing.
3. If the clips do not contain the answer, say so plainly and name the closest \
thing you did find. Never guess, never fill gaps from general knowledge, and \
never invent a clip id.
4. When the user asks for something they copied, reproduce it exactly — same \
characters, same formatting. Do not clean it up, summarize it, or correct it.
5. Be brief. Answer the question, cite, stop.";

fn generate(
    question: &str,
    context: &str,
    thread: &AskThread,
    cfg: &AskConfig,
    api: &TransformConfig,
) -> Result<(String, Option<Usage>), String> {
    let api_key = api
        .api_key
        .as_deref()
        .filter(|k| !k.is_empty())
        .ok_or("Ask requires an API key — set it in transform.json")?;

    let mut messages: Vec<serde_json::Value> = vec![serde_json::json!({
        "role": "system",
        "content": SYSTEM_PROMPT,
    })];

    // Replay recent turns so follow-ups ("and the one before it?") resolve.
    for turn in thread.turns.iter().rev().take(cfg.history_turns).rev() {
        messages.push(serde_json::json!({"role": "user", "content": turn.question}));
        messages.push(serde_json::json!({"role": "assistant", "content": turn.answer}));
    }

    messages.push(serde_json::json!({
        "role": "user",
        "content": format!(
            "Clips retrieved for this question:\n\n{}\n---\nQuestion: {}",
            context, question
        ),
    }));

    let body = serde_json::json!({
        "model": api.model,
        "messages": messages,
        "max_tokens": cfg.max_tokens,
        "temperature": cfg.temperature,
    });

    let response = ureq::post(&api.api_url)
        .set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {}", api_key))
        .send_json(body)
        .map_err(|e| format!("Ask API request failed: {}", e))?;

    let resp: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("Failed to parse ask response: {}", e))?;

    let text = resp["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| {
            resp["error"]["message"]
                .as_str()
                .map(|e| format!("API error: {}", e))
                .unwrap_or_else(|| "Unexpected ask response format".to_string())
        })?;

    let usage = resp.get("usage").map(|u| Usage {
        prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0),
        completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
        total_tokens: u["total_tokens"].as_u64().unwrap_or(0),
    });

    Ok((text, usage))
}

// ── Grounding enforcement ──

/// Pull `[#id]` citations out of the answer and split them into ids that were
/// really in the context and ids the model made up. Fabricated citations are
/// rewritten out of the text — rendering them would make a hallucination look
/// exactly like a real source.
fn resolve_citations(answer: &str, allowed: &[i64]) -> (String, Vec<i64>, Vec<i64>) {
    let mut cited: Vec<i64> = Vec::new();
    let mut invalid: Vec<i64> = Vec::new();
    let mut out = String::with_capacity(answer.len());

    let bytes: Vec<char> = answer.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '[' && i + 1 < bytes.len() && bytes[i + 1] == '#' {
            let mut j = i + 2;
            let mut digits = String::new();
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                digits.push(bytes[j]);
                j += 1;
            }
            if !digits.is_empty() && j < bytes.len() && bytes[j] == ']' {
                if let Ok(id) = digits.parse::<i64>() {
                    if allowed.contains(&id) {
                        if !cited.contains(&id) {
                            cited.push(id);
                        }
                        out.push_str(&format!("[#{}]", id));
                    } else {
                        if !invalid.contains(&id) {
                            invalid.push(id);
                        }
                        out.push_str("[unverified]");
                    }
                    i = j + 1;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }

    (out, cited, invalid)
}

fn score_confidence(
    answer: &str,
    sources: &[AskSource],
    retrieved: &[RetrievedClip],
) -> Confidence {
    if sources.is_empty() {
        let lowered = answer.to_lowercase();
        // The model saying it found nothing is a correct outcome, not a
        // low-quality one — but it isn't an answer either.
        if lowered.contains("couldn't find")
            || lowered.contains("could not find")
            || lowered.contains("don't have")
            || lowered.contains("no clip")
            || lowered.contains("nothing in")
        {
            return Confidence::None;
        }
        return Confidence::Low;
    }

    let multi_retriever = sources.iter().any(|s| {
        retrieved
            .iter()
            .find(|r| r.clip.id == s.clip_id)
            .map(|r| r.retriever_count() >= 2)
            .unwrap_or(false)
    });

    if multi_retriever {
        Confidence::High
    } else {
        Confidence::Medium
    }
}

// ── Helpers ──

/// Follow-ups inherit the previous question's vocabulary for retrieval only.
fn expand_query(question: &str, thread: &AskThread) -> String {
    let looks_like_followup = question.split_whitespace().count() <= 6
        || question.to_lowercase().starts_with("and ")
        || question.to_lowercase().starts_with("what about");

    match thread.turns.last() {
        Some(prev) if looks_like_followup => format!("{} {}", prev.question, question),
        _ => question.to_string(),
    }
}

fn significant_terms(query: &str) -> Vec<&str> {
    query
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '.')
        .filter(|w| w.len() >= 3)
        .filter(|w| !QUESTION_WORDS.contains(&w.to_lowercase().as_str()))
        .collect()
}

const QUESTION_WORDS: &[&str] = &[
    "what", "when", "where", "which", "who", "why", "how", "did", "was", "were", "the", "and",
    "for", "that", "this", "with", "from", "copy", "copied", "clipboard", "clip", "have", "has",
    "any", "get", "got", "show", "find", "about", "there", "their",
];

/// Rough token estimate for budgeting. ~4 chars/token is the usual English
/// approximation; code and JSON run denser, so this is a floor, not a promise.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max).collect();
    format!("{}\n… [clip truncated]", kept)
}

fn one_line(s: &str, max: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ContentType;

    fn clip(id: i64, content: &str) -> ClipEntry {
        ClipEntry {
            id,
            content: content.to_string(),
            content_type: ContentType::Text,
            content_hash: format!("h{}", id),
            source_app: Some("Chrome".into()),
            source_title: None,
            timestamp: Utc::now(),
            preview: content.chars().take(40).collect(),
            slot: None,
            image_path: None,
            thumb_path: None,
            ocr_text: None,
        }
    }

    /// Keyless config: retrieval only, no embedding retriever, no network.
    fn no_api() -> TransformConfig {
        TransformConfig {
            api_key: None,
            api_url: "http://127.0.0.1:1/v1/chat/completions".into(),
            model: "unused".into(),
        }
    }

    fn pool_of(clips: Vec<ClipEntry>) -> HashMap<i64, ClipEntry> {
        clips.into_iter().map(|c| (c.id, c)).collect()
    }

    #[test]
    fn rrf_rewards_agreement_over_a_single_top_hit() {
        let cfg = AskConfig::default();
        let pool = pool_of(vec![clip(1, "alpha"), clip(2, "beta")]);

        // Clip 2 is #1 for one retriever. Clip 1 is #2 for all three.
        let ranked = vec![
            (Retriever::FullText, vec![2, 1]),
            (Retriever::TfIdf, vec![3, 1]),
            (Retriever::Embedding, vec![3, 1]),
        ];

        let fused = fuse(ranked, pool, &cfg);
        assert_eq!(fused[0].clip.id, 1, "consensus should beat a lone #1");
        assert_eq!(fused[0].retriever_count(), 3);
    }

    #[test]
    fn fusion_records_which_retrievers_matched() {
        let cfg = AskConfig::default();
        let pool = pool_of(vec![clip(7, "stripe webhook payload")]);
        let ranked = vec![
            (Retriever::FullText, vec![7]),
            (Retriever::Embedding, vec![7]),
        ];

        let fused = fuse(ranked, pool, &cfg);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].matched_by(), "full-text + embedding");
    }

    #[test]
    fn fusion_respects_top_k() {
        let cfg = AskConfig {
            top_k: 2,
            ..Default::default()
        };
        let pool = pool_of(vec![clip(1, "a"), clip(2, "b"), clip(3, "c")]);
        let ranked = vec![(Retriever::FullText, vec![1, 2, 3])];
        assert_eq!(fuse(ranked, pool, &cfg).len(), 2);
    }

    #[test]
    fn valid_citations_are_kept() {
        let (text, cited, invalid) = resolve_citations("You copied it from [#42] earlier.", &[42]);
        assert_eq!(text, "You copied it from [#42] earlier.");
        assert_eq!(cited, vec![42]);
        assert!(invalid.is_empty());
    }

    #[test]
    fn fabricated_citations_are_stripped_not_rendered() {
        let (text, cited, invalid) = resolve_citations("From [#42] and [#999].", &[42]);
        assert_eq!(text, "From [#42] and [unverified].");
        assert_eq!(cited, vec![42]);
        assert_eq!(invalid, vec![999]);
    }

    #[test]
    fn repeated_citations_are_deduped() {
        let (_, cited, _) = resolve_citations("[#5] then [#5] again", &[5]);
        assert_eq!(cited, vec![5]);
    }

    #[test]
    fn non_citation_brackets_pass_through() {
        let (text, cited, _) = resolve_citations("array[#] and [#abc] stay put", &[1]);
        assert_eq!(text, "array[#] and [#abc] stay put");
        assert!(cited.is_empty());
    }

    #[test]
    fn confidence_is_high_only_with_cross_retriever_agreement() {
        let mut r = RetrievedClip {
            clip: clip(1, "x"),
            fused_score: 0.5,
            hits: vec![(Retriever::FullText, 1), (Retriever::TfIdf, 2)],
            withheld: None,
        };
        let src = vec![AskSource {
            clip_id: 1,
            preview: "x".into(),
            source_app: None,
            timestamp: Utc::now(),
            matched_by: "full-text + tf-idf".into(),
            fused_score: 0.5,
        }];
        assert_eq!(score_confidence("see [#1]", &src, &[r.clone()]), Confidence::High);

        r.hits = vec![(Retriever::FullText, 1)];
        assert_eq!(
            score_confidence("see [#1]", &src, &[r]),
            Confidence::Medium
        );
    }

    #[test]
    fn uncited_answer_is_low_confidence() {
        assert_eq!(score_confidence("It was probably JSON.", &[], &[]), Confidence::Low);
    }

    #[test]
    fn explicit_not_found_is_none_not_low() {
        assert_eq!(
            score_confidence("I couldn't find anything about that.", &[], &[]),
            Confidence::None
        );
    }

    #[test]
    fn context_blocks_carry_ids_and_provenance() {
        let cfg = AskConfig::default();
        let r = RetrievedClip {
            clip: clip(11, "SELECT * FROM users;"),
            fused_score: 0.3,
            hits: vec![(Retriever::FullText, 1)],
            withheld: None,
        };
        let ctx = build_context(&[&r], &cfg);
        assert!(ctx.contains("[#11]"));
        assert!(ctx.contains("Chrome"));
        assert!(ctx.contains("SELECT * FROM users;"));
    }

    #[test]
    fn context_stops_at_the_char_budget() {
        let cfg = AskConfig {
            max_context_chars: 200,
            ..Default::default()
        };
        let a = RetrievedClip {
            clip: clip(1, &"x".repeat(300)),
            fused_score: 0.3,
            hits: vec![(Retriever::FullText, 1)],
            withheld: None,
        };
        let b = RetrievedClip {
            clip: clip(2, &"y".repeat(300)),
            fused_score: 0.2,
            hits: vec![(Retriever::FullText, 2)],
            withheld: None,
        };
        let ctx = build_context(&[&a, &b], &cfg);
        // First block always goes in even when oversized; the second must not.
        assert!(ctx.contains("[#1]"));
        assert!(!ctx.contains("[#2]"));
    }

    #[test]
    fn long_clips_are_truncated_per_clip() {
        let cfg = AskConfig {
            max_clip_chars: 20,
            ..Default::default()
        };
        let r = RetrievedClip {
            clip: clip(1, &"z".repeat(500)),
            fused_score: 0.3,
            hits: vec![(Retriever::FullText, 1)],
            withheld: None,
        };
        assert!(build_context(&[&r], &cfg).contains("clip truncated"));
    }

    #[test]
    fn ocr_text_stands_in_for_empty_image_content() {
        let mut c = clip(1, "");
        c.ocr_text = Some("invoice total 42.00".into());
        assert_eq!(clip_body(&c), "invoice total 42.00");
    }

    #[test]
    fn followups_inherit_the_previous_question() {
        let thread = AskThread {
            id: None,
            turns: vec![AskTurn {
                question: "what was the stripe webhook payload".into(),
                answer: "…".into(),
                cited_ids: vec![],
                asked_at: Utc::now(),
            }],
        };
        let expanded = expand_query("and the one before?", &thread);
        assert!(expanded.contains("stripe webhook"));
        assert!(expanded.contains("the one before"));
    }

    #[test]
    fn long_standalone_questions_are_not_expanded() {
        let thread = AskThread {
            id: None,
            turns: vec![AskTurn {
                question: "stripe".into(),
                answer: "…".into(),
                cited_ids: vec![],
                asked_at: Utc::now(),
            }],
        };
        let q = "show me the postgres connection string I copied from DataGrip yesterday";
        assert_eq!(expand_query(q, &thread), q);
    }

    // End-to-end retrieval against a real SQLite store: FTS5 and TF-IDF both
    // run for real here, so this catches query-escaping and fusion-plumbing
    // breakage that the pure-fusion tests above cannot.
    fn seeded_store() -> ClipStore {
        let store = ClipStore::in_memory().unwrap();
        for (content, app) in [
            (
                "postgres://admin:hunter2@db.internal:5432/production",
                "DataGrip",
            ),
            (
                "{\"event\":\"payment_intent.succeeded\",\"id\":\"evt_1P\"}",
                "Chrome",
            ),
            ("SELECT id, email FROM users WHERE active = true;", "DataGrip"),
            ("brew install --cask docker", "Terminal"),
            ("The quick brown fox jumps over the lazy dog", "Notes"),
        ] {
            let mut entry = ClipEntry::new(content.to_string(), Some(app.to_string()), None);
            entry.id = 0;
            store.insert(&entry).unwrap();
        }
        store
    }

    #[test]
    fn end_to_end_retrieval_finds_the_right_clip() {
        let store = seeded_store();
        let cfg = AskConfig::default();

        let hits = retrieve(
            &store,
            "what was the postgres connection string",
            &AskFilters::default(),
            &cfg,
            &no_api(),
        )
        .unwrap();

        assert!(!hits.is_empty(), "retrieval returned nothing");
        assert!(
            hits[0].clip.content.starts_with("postgres://"),
            "expected the connection string first, got: {}",
            hits[0].clip.preview
        );
    }

    #[test]
    fn retrieval_narrows_by_source_app() {
        let store = seeded_store();
        let cfg = AskConfig::default();

        let hits = retrieve(
            &store,
            "docker",
            &AskFilters {
                source_app: Some("DataGrip".into()),
                since: None,
            },
            &cfg,
            &no_api(),
        )
        .unwrap();

        assert!(
            hits.iter().all(|h| h.clip.source_app.as_deref() == Some("DataGrip")),
            "app filter leaked clips from other apps"
        );
    }

    #[test]
    fn quotes_in_a_question_do_not_break_fts() {
        let store = seeded_store();
        // A raw double quote reaches the FTS5 MATCH expression; if it isn't
        // escaped this panics or errors instead of returning results.
        let hits = retrieve(
            &store,
            "what is \"payment_intent.succeeded\"?",
            &AskFilters::default(),
            &AskConfig::default(),
            &no_api(),
        )
        .unwrap();
        assert!(hits.iter().any(|h| h.clip.content.contains("payment_intent")));
    }

    #[test]
    fn secrets_are_withheld_from_the_prompt() {
        let store = seeded_store();
        let mut hits = retrieve(
            &store,
            "postgres connection string",
            &AskFilters::default(),
            &AskConfig::default(),
            &no_api(),
        )
        .unwrap();

        mark_secrets(&mut hits);

        let creds = hits
            .iter()
            .find(|h| h.clip.content.starts_with("postgres://"))
            .expect("connection string should have been retrieved");
        assert!(
            creds.withheld.is_some(),
            "a clip with embedded credentials must not reach the API"
        );
    }

    #[test]
    fn retrieval_on_an_empty_history_is_not_an_error() {
        let store = ClipStore::in_memory().unwrap();
        let hits = retrieve(
            &store,
            "anything at all",
            &AskFilters::default(),
            &AskConfig::default(),
            &no_api(),
        )
        .unwrap();
        assert!(hits.is_empty());
    }

    // ── Generation path, against a mock OpenAI-compatible endpoint ──
    //
    // Everything below the HTTP boundary is real: the assembled prompt, the
    // request body, citation validation and confidence scoring on the way
    // back. Only the model itself is faked.

    /// Serve exactly one chat-completion response, handing the request body
    /// back over a channel so the prompt can be asserted on.
    fn spawn_mock_llm(answer: &str) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let body = serde_json::json!({
            "choices": [{ "message": { "content": answer } }],
            "usage": { "prompt_tokens": 120, "completion_tokens": 30, "total_tokens": 150 },
        })
        .to_string();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            listener.local_addr().unwrap().port()
        );
        let (tx, rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            // Read headers, then exactly Content-Length bytes of body.
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            let mut content_len = None;
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => raw.extend_from_slice(&buf[..n]),
                    Err(_) => break,
                }
                let text = String::from_utf8_lossy(&raw).to_string();
                if let Some(split) = text.find("\r\n\r\n") {
                    if content_len.is_none() {
                        content_len = text[..split].lines().find_map(|line| {
                            let lower = line.to_ascii_lowercase();
                            lower
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        });
                    }
                    if raw.len() >= split + 4 + content_len.unwrap_or(0) {
                        let _ = tx.send(text[split + 4..].to_string());
                        break;
                    }
                }
            }

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });

        (url, rx)
    }

    /// Wait for the mock's captured request, failing loudly instead of
    /// hanging the whole test binary when no request was ever sent.
    fn recv_request(rx: &std::sync::mpsc::Receiver<String>) -> String {
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("expected an outbound request, but none was made")
    }

    fn mock_api(url: String) -> TransformConfig {
        TransformConfig {
            api_key: Some("test-key".into()),
            api_url: url,
            model: "mock-model".into(),
        }
    }

    #[test]
    fn generation_grounds_the_answer_and_drops_fabricated_citations() {
        let store = seeded_store();
        // Clip #3 is the SELECT statement and is genuinely retrievable;
        // #999 does not exist and must not survive as a citation.
        let (url, requests) = spawn_mock_llm("You ran [#3] against users, per [#999].");

        let answer = ask(
            &store,
            "what was that users query",
            &AskThread::new(),
            &AskFilters::default(),
            &AskConfig::default(),
            &mock_api(url),
        )
        .unwrap();

        assert!(!answer.retrieval_only, "a key was configured");
        assert_eq!(answer.answer, "You ran [#3] against users, per [unverified].");
        assert_eq!(answer.invalid_citations, vec![999]);
        assert_eq!(answer.sources.len(), 1);
        assert_eq!(answer.sources[0].clip_id, 3);
        assert_ne!(answer.confidence, Confidence::None);

        // The prompt the model actually received.
        let sent: serde_json::Value =
            serde_json::from_str(&recv_request(&requests)).unwrap();
        assert_eq!(sent["model"], "mock-model");
        assert_eq!(sent["temperature"], 0.1);
        assert_eq!(sent["max_tokens"], 1024);

        let system = sent["messages"][0]["content"].as_str().unwrap();
        assert_eq!(sent["messages"][0]["role"], "system");
        assert!(system.contains("Answer ONLY from the clips provided"));

        let user = sent["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(user.contains("[#3]"), "clip ids must be in the context");
        assert!(user.contains("SELECT id, email FROM users"));
    }

    #[test]
    fn usage_is_captured_from_the_response() {
        let store = seeded_store();
        let (url, _rx) = spawn_mock_llm("Nothing to report.");

        let answer = ask(
            &store,
            "docker install command",
            &AskThread::new(),
            &AskFilters::default(),
            &AskConfig::default(),
            &mock_api(url),
        )
        .unwrap();

        let usage = answer.usage.expect("usage should be parsed");
        assert_eq!(usage.total_tokens, 150);
        assert!(answer.estimated_prompt_tokens > 0);
    }

    #[test]
    fn credentials_never_reach_the_request_body() {
        let store = seeded_store();
        let (url, requests) = spawn_mock_llm("Found it in [#1].");

        let answer = ask(
            &store,
            "postgres users database query",
            &AskThread::new(),
            &AskFilters::default(),
            &AskConfig::default(),
            &mock_api(url),
        )
        .unwrap();

        assert!(answer.withheld_count > 0, "the credential clip was retrieved");

        let sent = recv_request(&requests);
        assert!(
            !sent.contains("hunter2"),
            "a withheld secret leaked into the outbound request"
        );
        // ...and the model citing the withheld clip must not resolve either.
        assert!(answer.sources.is_empty());
        assert_eq!(answer.invalid_citations, vec![1]);
    }

    #[test]
    fn prior_turns_are_replayed_to_the_model() {
        let store = seeded_store();
        let (url, requests) = spawn_mock_llm("The one before was [#3].");

        let thread = AskThread {
            id: None,
            turns: vec![AskTurn {
                question: "what docker command did I copy".into(),
                answer: "You copied [#4].".into(),
                cited_ids: vec![4],
                asked_at: Utc::now(),
            }],
        };

        ask(
            &store,
            "and the one before?",
            &thread,
            &AskFilters::default(),
            &AskConfig::default(),
            &mock_api(url),
        )
        .unwrap();

        let sent: serde_json::Value =
            serde_json::from_str(&recv_request(&requests)).unwrap();
        let messages = sent["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4, "system + prior pair + new question");
        assert_eq!(messages[1]["role"], "user");
        assert!(messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("what docker command"));
        assert_eq!(messages[2]["role"], "assistant");
    }

    #[test]
    fn a_blank_key_skips_the_network_entirely() {
        let store = seeded_store();
        // Port 1 is not listening; if generation were attempted this errors
        // instead of returning a clean retrieval-only answer.
        let api = TransformConfig {
            api_key: None,
            api_url: "http://127.0.0.1:1/v1/chat/completions".into(),
            model: "unused".into(),
        };

        let answer = ask(
            &store,
            "anything",
            &AskThread::new(),
            &AskFilters::default(),
            &AskConfig::default(),
            &api,
        )
        .unwrap();

        assert!(answer.retrieval_only);
        assert!(answer.answer.is_empty());
        assert_eq!(answer.confidence, Confidence::None);
    }

    #[test]
    fn an_api_error_surfaces_rather_than_inventing_an_answer() {
        let store = seeded_store();
        let api = TransformConfig {
            api_key: Some("k".into()),
            api_url: "http://127.0.0.1:1/v1/chat/completions".into(),
            model: "m".into(),
        };

        let result = ask(
            &store,
            "docker",
            &AskThread::new(),
            &AskFilters::default(),
            &AskConfig::default(),
            &api,
        );
        assert!(result.is_err(), "a dead endpoint must not yield an answer");
    }

    #[test]
    fn significant_terms_drop_question_scaffolding() {
        let terms = significant_terms("what was the stripe webhook secret I copied");
        assert!(terms.contains(&"stripe"));
        assert!(terms.contains(&"webhook"));
        assert!(!terms.contains(&"what"));
        assert!(!terms.contains(&"copied"));
    }
}
