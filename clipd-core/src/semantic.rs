use std::collections::HashMap;

/// A search result with relevance score.
#[derive(Debug, Clone)]
pub struct SemanticResult {
    pub clip_index: usize,
    pub score: f64,
}

/// TF-IDF index for meaning-based search over clipboard history.
pub struct TfIdfIndex {
    doc_terms: Vec<HashMap<String, f64>>,
    idf: HashMap<String, f64>,
    doc_count: usize,
}

impl TfIdfIndex {
    /// Build an index from a list of text documents.
    /// Each document is a clip's content string.
    pub fn build(documents: &[&str]) -> Self {
        let n = documents.len();
        if n == 0 {
            return Self {
                doc_terms: Vec::new(),
                idf: HashMap::new(),
                doc_count: 0,
            };
        }

        let mut doc_terms: Vec<HashMap<String, f64>> = Vec::with_capacity(n);
        let mut doc_freq: HashMap<String, usize> = HashMap::new();

        for doc in documents {
            let tokens = tokenize(doc);
            let total = tokens.len() as f64;
            let mut tf: HashMap<String, f64> = HashMap::new();

            for token in &tokens {
                *tf.entry(token.clone()).or_insert(0.0) += 1.0;
            }

            let unique_terms: Vec<String> = tf.keys().cloned().collect();
            for term in &unique_terms {
                if let Some(f) = tf.get_mut(term) {
                    *f /= total.max(1.0);
                }
            }

            for term in &unique_terms {
                *doc_freq.entry(term.clone()).or_insert(0) += 1;
            }

            doc_terms.push(tf);
        }

        let mut idf: HashMap<String, f64> = HashMap::new();
        for (term, df) in &doc_freq {
            idf.insert(term.clone(), ((n as f64) / (*df as f64 + 1.0)).ln() + 1.0);
        }

        Self {
            doc_terms,
            idf,
            doc_count: n,
        }
    }

    /// Search the index with a natural language query.
    /// Returns results sorted by relevance (highest first).
    pub fn search(&self, query: &str, max_results: usize) -> Vec<SemanticResult> {
        if self.doc_count == 0 {
            return Vec::new();
        }

        let query_tokens = tokenize(query);
        if query_tokens.is_empty() {
            return Vec::new();
        }

        let mut query_vec: HashMap<String, f64> = HashMap::new();
        let total = query_tokens.len() as f64;
        for token in &query_tokens {
            *query_vec.entry(token.clone()).or_insert(0.0) += 1.0;
        }
        for val in query_vec.values_mut() {
            *val /= total;
        }

        let mut results: Vec<SemanticResult> = self
            .doc_terms
            .iter()
            .enumerate()
            .map(|(idx, doc_tf)| {
                let score = cosine_similarity(&query_vec, doc_tf, &self.idf);
                SemanticResult {
                    clip_index: idx,
                    score,
                }
            })
            .filter(|r| r.score > 0.001)
            .collect();

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(max_results);
        results
    }
}

fn cosine_similarity(
    query: &HashMap<String, f64>,
    doc: &HashMap<String, f64>,
    idf: &HashMap<String, f64>,
) -> f64 {
    let mut dot = 0.0f64;
    let mut q_norm = 0.0f64;
    let mut d_norm = 0.0f64;

    for (term, q_tf) in query {
        let idf_val = idf.get(term).copied().unwrap_or(0.0);
        let q_tfidf = q_tf * idf_val;
        q_norm += q_tfidf * q_tfidf;

        if let Some(d_tf) = doc.get(term) {
            let d_tfidf = d_tf * idf_val;
            dot += q_tfidf * d_tfidf;
        }
    }

    for (term, d_tf) in doc {
        let idf_val = idf.get(term).copied().unwrap_or(0.0);
        let d_tfidf = d_tf * idf_val;
        d_norm += d_tfidf * d_tfidf;
    }

    let denom = q_norm.sqrt() * d_norm.sqrt();
    if denom < 1e-10 {
        0.0
    } else {
        dot / denom
    }
}

/// Tokenize text into lowercase words, stripping punctuation.
/// Also generates bigrams for better semantic matching.
fn tokenize(text: &str) -> Vec<String> {
    let words: Vec<String> = text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() >= 2)
        .filter(|w| !STOP_WORDS.contains(w))
        .map(String::from)
        .collect();

    let mut tokens = words.clone();

    // Add bigrams for better context matching
    for pair in words.windows(2) {
        tokens.push(format!("{}_{}", pair[0], pair[1]));
    }

    tokens
}

const STOP_WORDS: &[&str] = &[
    "the", "is", "at", "which", "on", "a", "an", "and", "or", "but", "in", "with", "to", "for",
    "of", "not", "no", "can", "had", "has", "have", "it", "be", "was", "were", "been", "are",
    "do", "does", "did", "will", "would", "could", "should", "may", "might", "shall", "this",
    "that", "these", "those", "he", "she", "we", "they", "you", "me", "him", "her", "us",
    "them", "my", "your", "his", "its", "our", "their", "what", "where", "when", "how", "why",
    "if", "then", "else", "from", "up", "out", "so", "as", "by", "about", "into", "just",
    "also", "than", "very", "too", "only",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_search() {
        let docs = vec![
            "postgresql database error connection refused",
            "javascript react component rendering issue",
            "aws s3 bucket permission denied access key",
            "python django rest framework api endpoint",
        ];
        let index = TfIdfIndex::build(&docs);

        let results = index.search("postgres database error", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].clip_index, 0);
    }

    #[test]
    fn test_semantic_match() {
        let docs = vec![
            "error connecting to the database server timeout",
            "beautiful sunset over the ocean waves",
            "mysql connection pool exhausted retry failed",
        ];
        let index = TfIdfIndex::build(&docs);

        let results = index.search("database connection problem", 5);
        assert!(results.len() >= 2);
        let indices: Vec<usize> = results.iter().map(|r| r.clip_index).collect();
        assert!(indices.contains(&0));
        assert!(indices.contains(&2));
    }

    #[test]
    fn test_empty_index() {
        let index = TfIdfIndex::build(&[]);
        let results = index.search("anything", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_no_match() {
        let docs = vec!["hello world", "foo bar baz"];
        let index = TfIdfIndex::build(&docs);
        let results = index.search("quantum physics", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_tokenize_filters_stop_words() {
        let tokens = tokenize("the quick brown fox jumps over the lazy dog");
        assert!(!tokens.contains(&"the".to_string()));
        assert!(tokens.contains(&"quick".to_string()));
        assert!(tokens.contains(&"brown".to_string()));
    }
}
