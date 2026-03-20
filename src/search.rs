use crate::parser::is_noise;
use crate::scoring::*;
use crate::session::*;
use chrono::{DateTime, FixedOffset, Utc};
use rayon::prelude::*;
use std::collections::{BTreeSet, HashMap};

const MAX_MATCHES_PER_SESSION: usize = 3;
const INDEX_QUALITY_THRESHOLD: f64 = 5.0;

pub struct IndexResult {
    pub session: Session,
    pub score: f64,
    pub matched_field: String,
    pub display: String,
}

pub struct SearchResult {
    pub session: Session,
    pub message: Message,
}

pub fn index_search(sessions: &[Session], query: &str, limit: usize) -> Vec<IndexResult> {
    let query_terms: Vec<String> = query.to_lowercase().split_whitespace().map(String::from).collect();
    if query_terms.is_empty() {
        return Vec::new();
    }

    let field_weights: &[(&str, f64)] = &[
        ("summary", 3.0),
        ("first_prompt", 2.0),
        ("branch", 1.0),
        ("project", 1.0),
    ];

    let mut scored: Vec<IndexResult> = Vec::new();

    for s in sessions {
        let mut total_score = 0.0;
        let mut best_field = "";
        let mut best_weight = 0.0;

        let field_values: [(&str, &str); 4] = [
            ("summary", &s.summary),
            ("first_prompt", &s.first_prompt),
            ("branch", &s.branch),
            ("project", &s.project),
        ];

        let mut all_found = true;
        for term in &query_terms {
            let mut term_found = false;
            for (fname, fval, weight) in field_values.iter().zip(field_weights.iter()).map(|((n, v), (_, w))| (n, v, w)) {
                if fval.to_lowercase().contains(term.as_str()) {
                    term_found = true;
                    total_score += weight;
                    if *weight > best_weight {
                        best_weight = *weight;
                        best_field = fname;
                    }
                }
            }
            if !term_found {
                all_found = false;
                break;
            }
        }

        if !all_found || total_score <= 0.0 {
            continue;
        }

        let ts = if s.modified.is_empty() { &s.created } else { &s.modified };
        total_score *= recency_multiplier(ts);

        let display = match best_field {
            "summary" => &s.summary,
            "first_prompt" => &s.first_prompt,
            "branch" => &s.branch,
            "project" => &s.project,
            _ => &s.summary,
        };
        let display = if display.is_empty() {
            if !s.summary.is_empty() { &s.summary } else { &s.first_prompt }
        } else {
            display
        };

        scored.push(IndexResult {
            session: s.clone(),
            score: total_score,
            matched_field: best_field.to_string(),
            display: display.chars().take(200).collect(),
        });
    }

    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    scored
}

pub fn index_quality_ok(results: &[IndexResult]) -> bool {
    results.first().map_or(false, |r| r.score >= INDEX_QUALITY_THRESHOLD)
}

fn parse_timeframe_duration(tf: &str) -> chrono::Duration {
    let lower = tf.to_lowercase();
    match lower.as_str() {
        "today" | "1d" => chrono::Duration::days(1),
        "yesterday" | "2d" => chrono::Duration::days(2),
        "week" | "7d" => chrono::Duration::days(7),
        "month" | "30d" => chrono::Duration::days(30),
        _ => {
            if let Some(n) = lower.strip_suffix('d').and_then(|s| s.parse::<i64>().ok()) {
                chrono::Duration::days(n)
            } else {
                chrono::Duration::days(365)
            }
        }
    }
}

fn parse_timestamp(ts: &str) -> Option<DateTime<FixedOffset>> {
    let s = ts.replace('Z', "+00:00");
    DateTime::parse_from_rfc3339(&s)
        .or_else(|_| DateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        .ok()
        .or_else(|| s.parse::<DateTime<Utc>>().ok().map(|t| t.fixed_offset()))
}

pub fn scored_search(
    sessions: &[Session],
    query: &str,
    scope: &str,
    limit: usize,
    timeframe: Option<&str>,
) -> Vec<SearchResult> {
    if is_uuid(query) {
        if let Some(s) = sessions.iter().find(|s| s.id == query.trim()) {
            let (messages, _) = parse_session(s, false);
            if let Some(mut msg) = messages.into_iter().next() {
                msg.final_score = 100.0;
                return vec![SearchResult { session: s.clone(), message: msg }];
            }
            let stub = Message {
                uuid: String::new(),
                timestamp: String::new(),
                role: "user".into(),
                content: if !s.summary.is_empty() { s.summary.clone() } else { s.first_prompt.clone() },
                session_id: s.id.clone(),
                project_path: s.project.clone(),
                tool_uses: Vec::new(),
                files_referenced: Vec::new(),
                error_patterns: Vec::new(),
                relevance_score: 0.0,
                final_score: 100.0,
            };
            return vec![SearchResult { session: s.clone(), message: stub }];
        }
        return Vec::new();
    }

    let tf_cutoff: Option<DateTime<FixedOffset>> = timeframe.map(|tf| {
        let dur = parse_timeframe_duration(tf);
        (Utc::now() - dur).fixed_offset()
    });

    let boosts = semantic_boosts(query);
    let raw_words: Vec<String> = {
        let deduped: BTreeSet<String> = query.to_lowercase().split_whitespace().map(String::from).collect();
        deduped.into_iter().collect()
    };
    let query_terms: Vec<String> = {
        let long: Vec<String> = raw_words.iter().filter(|w| w.len() > 2).cloned().collect();
        if long.is_empty() {
            raw_words.iter().filter(|w| w.len() >= 2).cloned().collect()
        } else {
            long
        }
    };
    let query_normalized = normalize_for_search(query);
    let query_words_norm: Vec<String> = {
        let deduped: BTreeSet<String> = query_normalized.split_whitespace()
            .filter(|w| w.len() > 2)
            .map(String::from)
            .collect();
        if deduped.is_empty() {
            query_normalized.split_whitespace()
                .filter(|w| w.len() >= 2)
                .map(String::from)
                .collect::<BTreeSet<String>>()
                .into_iter()
                .collect()
        } else {
            deduped.into_iter().collect()
        }
    };
    let query_words_norm_refs: Vec<&str> = query_words_norm.iter().map(|s| s.as_str()).collect();

    let use_similar = scope == "similar";

    let candidates: Vec<(Session, Message, String)> = sessions
        .par_iter()
        .filter(|s| !s.file.is_empty() && std::path::Path::new(&s.file).exists())
        .flat_map(|s| {
            let (messages, _) = parse_session(s, false);
            let mut hits = Vec::new();
            for mut msg in messages {
                let cl = msg.content_lower();
                if is_noise(&cl) {
                    continue;
                }
                if scope == "errors" && msg.error_patterns.is_empty() && !cl.contains("error") {
                    continue;
                }
                if use_similar && msg.role != "user" {
                    continue;
                }
                if scope == "tools" && msg.tool_uses.is_empty() {
                    continue;
                }
                if scope == "files" && msg.files_referenced.is_empty() {
                    continue;
                }

                if let Some(cutoff) = tf_cutoff {
                    if !msg.timestamp.is_empty() {
                        if let Some(ts) = parse_timestamp(&msg.timestamp) {
                            if ts < cutoff {
                                continue;
                            }
                        }
                    }
                }

                if use_similar {
                    let sim = query_similarity(query, &msg.content);
                    if sim < 0.25 {
                        continue;
                    }
                    msg.relevance_score = sim * 10.0;
                } else {
                    let hist_score = score_relevance(&msg.content, query);
                    let normalized = normalize_for_search(&msg.content);
                    let prefix_score = prefix_match_score(&normalized, &query_words_norm_refs, &msg.timestamp);
                    msg.relevance_score = hist_score + prefix_score * 2.0;
                }
                msg.session_id = s.id.clone();
                msg.project_path = s.project.clone();
                hits.push((s.clone(), msg, cl));
            }
            hits
        })
        .collect();

    let mut results: Vec<(Session, Message)> = candidates
        .into_iter()
        .map(|(s, mut msg, cl)| {
            let mut score = msg.relevance_score;
            let match_count = query_terms.iter().filter(|t| cl.contains(t.as_str())).count();

            if query_terms.len() >= 2 && score == 0.0 && match_count == 0 {
                msg.final_score = 0.0;
                return (s, msg);
            }

            if match_count > 0 {
                score *= (1.0 + 0.5 * match_count as f64).min(MAX_MULTIPLICATIVE_BOOST);
            } else {
                score *= 0.1;
            }

            for (btype, bval) in &boosts {
                match *btype {
                    "error_resolution" if cl.contains("error") || cl.contains("exception") => score *= bval,
                    "solutions" if cl.contains("fix") || cl.contains("resolve") => score *= bval,
                    "implementation" if cl.contains("implement") || cl.contains("create") => score *= bval,
                    "optimization" if cl.contains("optimiz") || cl.contains("performance") || cl.contains("improve") => score *= bval,
                    "file_operations" if cl.contains("file") || cl.contains("read") || cl.contains("write") => score *= bval,
                    "tool_usage" if cl.contains("tool") || !msg.tool_uses.is_empty() => score *= bval,
                    _ => {}
                }
            }

            score *= importance_boost(&cl);

            if !msg.timestamp.is_empty() {
                let recency = recency_multiplier(&msg.timestamp);
                if recency >= 3.0 {
                    score *= 1.5;
                } else if recency >= 2.0 {
                    score *= 1.2;
                } else if recency >= 1.5 {
                    score *= 1.1;
                }
            }

            if !msg.tool_uses.is_empty() {
                score *= 1.3;
            }
            if !msg.files_referenced.is_empty() {
                score *= 1.2;
            }
            if !msg.error_patterns.is_empty() {
                score *= 1.4;
            }
            if msg.role == "assistant"
                && (cl.contains("solution") || cl.contains("fixed") || cl.contains("resolved"))
            {
                score *= 1.6;
            }

            msg.final_score = score;
            (s, msg)
        })
        .collect();

    // Deduplicate
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut deduped: Vec<(Session, Message)> = Vec::new();
    results.sort_by(|a, b| b.1.final_score.partial_cmp(&a.1.final_score).unwrap_or(std::cmp::Ordering::Equal));
    for (s, msg) in results {
        if msg.final_score <= 0.0 {
            continue;
        }
        let sig = content_signature(&msg.content, &msg.tool_uses, &msg.files_referenced);
        if let Some(idx) = seen.get(&sig) {
            if msg.final_score > deduped[*idx].1.final_score {
                deduped[*idx] = (s, msg);
            }
        } else {
            seen.insert(sig, deduped.len());
            deduped.push((s, msg));
        }
    }

    // Per-session cap
    let mut session_counts: HashMap<String, usize> = HashMap::new();
    let mut capped = Vec::new();
    for (s, m) in deduped {
        let count = session_counts.entry(s.id.clone()).or_insert(0);
        if *count < MAX_MATCHES_PER_SESSION {
            *count += 1;
            capped.push((s, m));
        }
    }

    // Quality gate with fallback (matches Python behavior)
    let quality: Vec<(Session, Message)> = capped
        .iter()
        .filter(|(_, m)| m.final_score >= 0.5 && m.content.len() >= 40)
        .cloned()
        .collect();
    let mut final_results = if quality.is_empty() {
        capped
    } else {
        quality
    };
    final_results.truncate(limit);
    final_results
        .into_iter()
        .map(|(session, message)| SearchResult { session, message })
        .collect()
}
