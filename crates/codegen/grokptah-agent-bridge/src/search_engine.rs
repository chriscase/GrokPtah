//! Hybrid search over chats + build sessions.
//!
//! ## Modes
//! - **Keyword** — token match + BM25-ish ranking with snippets
//! - **Semantic** — TF–IDF cosine similarity over session/message bags
//!   (offline, no embedding API required). Optional remote embeddings can be
//!   layered later without changing the query API.
//! - **Hybrid** — weighted blend of keyword + semantic scores
//!
//! ## Scope
//! Index covers session title, folder, tags, cwd, and full transcript text
//! for both `chat` and `build` kinds (filterable).

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::session::{SessionKind, TranscriptEntry};
use crate::session_store::{self, SessionMeta};

const STOP: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "in", "on", "at", "to", "for", "of", "is", "it",
    "this", "that", "with", "as", "by", "from", "be", "are", "was", "were", "been", "have",
    "has", "had", "do", "does", "did", "will", "would", "could", "should", "may", "might",
    "not", "no", "yes", "you", "your", "i", "me", "my", "we", "our", "they", "them", "their",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Keyword,
    Semantic,
    Hybrid,
}

impl SearchMode {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "keyword" | "kw" | "text" | "fts" => Self::Keyword,
            "semantic" | "sem" | "vector" => Self::Semantic,
            _ => Self::Hybrid,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    pub query: String,
    #[serde(default)]
    pub mode: String,
    /// `all` | `chat` | `build`
    #[serde(default = "default_kind_filter")]
    pub kind: String,
    #[serde(default)]
    pub include_archived: bool,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
}

fn default_kind_filter() -> String {
    "all".into()
}
fn default_limit() -> usize {
    40
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub session_id: Uuid,
    pub title: String,
    pub kind: SessionKind,
    pub folder: Option<String>,
    pub tags: Vec<String>,
    pub archived: bool,
    pub score: f32,
    pub keyword_score: f32,
    pub semantic_score: f32,
    pub snippet: String,
    pub match_field: String,
    pub message_index: Option<usize>,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
struct Doc {
    session_id: Uuid,
    meta: SessionMeta,
    /// Concatenated searchable text
    body: String,
    /// Per-message bodies for snippet/message hits
    messages: Vec<(usize, String, String)>, // index, role, text
    tokens: Vec<String>,
    tf: HashMap<String, f32>,
}

/// Run search across all on-disk sessions (loads transcripts as needed).
pub fn search(q: &SearchQuery) -> anyhow::Result<Vec<SearchHit>> {
    let mode = SearchMode::parse(&q.mode);
    let query = q.query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let q_tokens = tokenize(query);
    if q_tokens.is_empty() {
        return Ok(Vec::new());
    }

    let metas = list_metas()?;
    let kind_filter = q.kind.trim().to_lowercase();
    let mut docs = Vec::new();
    for m in metas {
        if !q.include_archived && m.archived {
            continue;
        }
        if kind_filter == "chat" && m.kind != SessionKind::Chat {
            continue;
        }
        if kind_filter == "build" && m.kind != SessionKind::Build {
            continue;
        }
        if let Some(f) = &q.folder {
            if m.folder.as_deref() != Some(f.as_str()) {
                continue;
            }
        }
        if let Some(t) = &q.tag {
            if !m.tags.iter().any(|x| x == t) {
                continue;
            }
        }
        if let Some(doc) = build_doc(m) {
            docs.push(doc);
        }
    }

    if docs.is_empty() {
        return Ok(Vec::new());
    }

    let n_docs = docs.len() as f32;
    // Document frequency
    let mut df: HashMap<String, f32> = HashMap::new();
    for d in &docs {
        let unique: HashSet<_> = d.tokens.iter().cloned().collect();
        for t in unique {
            *df.entry(t).or_default() += 1.0;
        }
    }

    let idf: HashMap<String, f32> = df
        .iter()
        .map(|(t, c)| {
            let v = ((n_docs - c + 0.5) / (c + 0.5) + 1.0).ln().max(0.0);
            (t.clone(), v)
        })
        .collect();

    // Query TF-IDF vector
    let mut q_tf: HashMap<String, f32> = HashMap::new();
    for t in &q_tokens {
        *q_tf.entry(t.clone()).or_default() += 1.0;
    }
    let q_len = q_tokens.len() as f32;
    for v in q_tf.values_mut() {
        *v /= q_len;
    }
    let q_vec: HashMap<String, f32> = q_tf
        .iter()
        .map(|(t, tf)| (t.clone(), tf * idf.get(t).copied().unwrap_or(0.0)))
        .collect();

    let mut hits = Vec::new();
    for d in &docs {
        let kw = keyword_score(&q_tokens, d, &idf);
        let sem = cosine(&q_vec, &tfidf_vec(&d.tf, &idf));
        let score = match mode {
            SearchMode::Keyword => kw,
            SearchMode::Semantic => sem,
            SearchMode::Hybrid => 0.55 * kw + 0.45 * sem,
        };
        if score <= 0.0001 && mode != SearchMode::Semantic {
            // still allow semantic-only weak hits above threshold
            if mode == SearchMode::Keyword {
                continue;
            }
        }
        if score <= 0.02 && mode == SearchMode::Semantic {
            continue;
        }
        if score <= 0.015 && mode == SearchMode::Hybrid {
            continue;
        }

        let (snippet, match_field, msg_idx) = best_snippet(d, &q_tokens);
        hits.push(SearchHit {
            session_id: d.session_id,
            title: d.meta.title.clone(),
            kind: d.meta.kind,
            folder: d.meta.folder.clone(),
            tags: d.meta.tags.clone(),
            archived: d.meta.archived,
            score,
            keyword_score: kw,
            semantic_score: sem,
            snippet,
            match_field,
            message_index: msg_idx,
            updated_at: d.meta.updated_at.to_rfc3339(),
        });
    }

    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(q.limit.max(1).min(200));
    Ok(hits)
}

fn list_metas() -> anyhow::Result<Vec<SessionMeta>> {
    // Reuse store private listing via public load_all_metas shells + re-read meta is heavy;
    // walk sessions dir.
    let root = session_store::sessions_root();
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let meta_p = entry.path().join("meta.json");
        if !meta_p.is_file() {
            continue;
        }
        let raw = std::fs::read_to_string(&meta_p)?;
        if let Ok(m) = serde_json::from_str::<SessionMeta>(&raw) {
            out.push(m);
        }
    }
    Ok(out)
}

fn build_doc(meta: SessionMeta) -> Option<Doc> {
    let path = session_store::sessions_root()
        .join(meta.id.to_string())
        .join("transcript.jsonl");
    let mut messages = Vec::new();
    let mut body_parts = vec![
        meta.title.clone(),
        meta.cwd.clone(),
        meta.folder.clone().unwrap_or_default(),
        meta.tags.join(" "),
        meta.kind.as_str().to_string(),
    ];
    if path.is_file() {
        if let Ok(f) = File::open(&path) {
            for (i, line) in BufReader::new(f).lines().enumerate() {
                let Ok(line) = line else { continue };
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(e) = serde_json::from_str::<TranscriptEntry>(&line) {
                    body_parts.push(e.text.clone());
                    messages.push((i, e.role, e.text));
                }
            }
        }
    }
    let body = body_parts.join("\n");
    let tokens = tokenize(&body);
    if tokens.is_empty() && meta.title.is_empty() {
        return None;
    }
    let mut tf = HashMap::new();
    let n = tokens.len().max(1) as f32;
    for t in &tokens {
        *tf.entry(t.clone()).or_default() += 1.0;
    }
    for v in tf.values_mut() {
        *v /= n;
    }
    Some(Doc {
        session_id: meta.id,
        meta,
        body,
        messages,
        tokens,
        tf,
    })
}

fn tokenize(s: &str) -> Vec<String> {
    let stop: HashSet<_> = STOP.iter().copied().collect();
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|t| t.len() > 1 && !stop.contains(t))
        .map(|t| t.to_string())
        .collect()
}

fn keyword_score(q_tokens: &[String], d: &Doc, idf: &HashMap<String, f32>) -> f32 {
    // BM25-ish over document tokens
    let mut score = 0.0f32;
    let avgdl = 200.0f32;
    let dl = d.tokens.len() as f32;
    let k1 = 1.2f32;
    let b = 0.75f32;
    let mut tf: HashMap<&str, f32> = HashMap::new();
    for t in &d.tokens {
        *tf.entry(t.as_str()).or_default() += 1.0;
    }
    // Title boost
    let title_toks: HashSet<_> = tokenize(&d.meta.title).into_iter().collect();
    for qt in q_tokens {
        let f = *tf.get(qt.as_str()).unwrap_or(&0.0);
        if f <= 0.0 {
            continue;
        }
        let idf_t = idf.get(qt).copied().unwrap_or(0.5);
        let denom = f + k1 * (1.0 - b + b * dl / avgdl);
        let mut s = idf_t * (f * (k1 + 1.0)) / denom;
        if title_toks.contains(qt) {
            s *= 2.2;
        }
        if d.meta.tags.iter().any(|t| t.eq_ignore_ascii_case(qt)) {
            s *= 1.6;
        }
        score += s;
    }
    // Phrase bonus
    if !q_tokens.is_empty() {
        let phrase = q_tokens.join(" ");
        if d.body.to_lowercase().contains(&phrase) {
            score *= 1.35;
        }
    }
    score
}

fn tfidf_vec(tf: &HashMap<String, f32>, idf: &HashMap<String, f32>) -> HashMap<String, f32> {
    tf.iter()
        .map(|(t, f)| (t.clone(), f * idf.get(t).copied().unwrap_or(0.0)))
        .collect()
}

fn cosine(a: &HashMap<String, f32>, b: &HashMap<String, f32>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (k, va) in a {
        na += va * va;
        if let Some(vb) = b.get(k) {
            dot += va * vb;
        }
    }
    for vb in b.values() {
        nb += vb * vb;
    }
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

fn best_snippet(d: &Doc, q_tokens: &[String]) -> (String, String, Option<usize>) {
    // Prefer a message containing query tokens
    let mut best: Option<(f32, usize, String, String)> = None;
    for (idx, role, text) in &d.messages {
        let lower = text.to_lowercase();
        let mut hits = 0f32;
        for t in q_tokens {
            if lower.contains(t) {
                hits += 1.0;
            }
        }
        if hits <= 0.0 {
            continue;
        }
        let snip = snippet_around(text, q_tokens, 140);
        if best.as_ref().map(|(s, _, _, _)| hits > *s).unwrap_or(true) {
            best = Some((hits, *idx, format!("message:{role}"), snip));
        }
    }
    if let Some((_, idx, field, snip)) = best {
        return (snip, field, Some(idx));
    }
    // Fall back to title / meta
    if q_tokens
        .iter()
        .any(|t| d.meta.title.to_lowercase().contains(t))
    {
        return (d.meta.title.clone(), "title".into(), None);
    }
    let body_snip = snippet_around(&d.body, q_tokens, 140);
    (body_snip, "body".into(), None)
}

fn snippet_around(text: &str, q_tokens: &[String], width: usize) -> String {
    let lower = text.to_lowercase();
    let mut pos = None;
    for t in q_tokens {
        if let Some(p) = lower.find(t) {
            pos = Some(p);
            break;
        }
    }
    let p = pos.unwrap_or(0);
    let start = p.saturating_sub(width / 3);
    let end = (p + width).min(text.len());
    // byte-safe trim
    let start = floor_char_boundary(text, start);
    let end = ceil_char_boundary(text, end);
    let mut s = text[start..end].to_string();
    if start > 0 {
        s = format!("…{s}");
    }
    if end < text.len() {
        s = format!("{s}…");
    }
    s.replace('\n', " ")
}

fn floor_char_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut i = i;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut i = i;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}
