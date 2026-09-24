use crate::parser::is_noise;
use crate::scoring::*;
use crate::session::*;
use chrono::{DateTime, FixedOffset, Utc};
use rayon::prelude::*;
use std::collections::{BTreeSet, HashMap};

const MAX_MATCHES_PER_SESSION: usize = 3;
const MAX_TOTAL_BOOST: f64 = 10.0;

pub struct SearchResult {
    pub session: Session,
    pub message: Message,
    /// Match-aware excerpt from the ranked passage, when the backend provides it.
    pub snippet: Option<String>,
    /// Position of `message` in the parsed transcript, as `view --around`
    /// takes it. None for title/prompt rows and session stubs.
    pub ordinal: Option<usize>,
    /// Up to two further matches when the request groups by conversation.
    pub additional_matches: Vec<SearchMatch>,
}

pub struct SearchMatch {
    pub message: Message,
    pub snippet: Option<String>,
    pub ordinal: Option<usize>,
}

/// Preserve first-hit order while collapsing message results into conversations.
pub fn group_results(results: Vec<SearchResult>, limit: usize) -> Vec<SearchResult> {
    let mut groups: HashMap<(String, String), usize> = HashMap::new();
    let mut grouped: Vec<SearchResult> = Vec::new();
    for hit in results {
        let key = (hit.session.source.clone(), hit.session.id.to_lowercase());
        if let Some(&index) = groups.get(&key) {
            if grouped[index].additional_matches.len() < 2 {
                grouped[index].additional_matches.push(SearchMatch {
                    message: hit.message,
                    snippet: hit.snippet,
                    ordinal: hit.ordinal,
                });
            }
        } else if grouped.len() < limit {
            groups.insert(key, grouped.len());
            grouped.push(hit);
        }
    }
    grouped
}

pub fn parse_timeframe_duration(tf: &str) -> Result<chrono::Duration, String> {
    let lower = tf.to_lowercase();
    let days = match lower.as_str() {
        "today" | "1d" => 1,
        "yesterday" | "2d" => 2,
        "week" | "7d" => 7,
        "month" | "30d" => 30,
        _ => {
            let Some(n) = lower.strip_suffix('d').and_then(|s| s.parse::<i64>().ok()) else {
                return Err(format!(
                    "invalid --timeframe '{tf}'; use today, yesterday, week, month, or Nd"
                ));
            };
            n
        }
    };
    if !(1..=36_500).contains(&days) {
        return Err(format!(
            "invalid --timeframe '{tf}'; N in Nd must be between 1 and 36500"
        ));
    }
    chrono::Duration::try_days(days).ok_or_else(|| format!("invalid --timeframe '{tf}'"))
}

fn timeframe_cutoff_fixed(tf: &str) -> Option<DateTime<FixedOffset>> {
    parse_timeframe_duration(tf).ok().map(|dur| {
        Utc::now()
            .checked_sub_signed(dur)
            .unwrap_or(DateTime::<Utc>::MIN_UTC)
            .fixed_offset()
    })
}

fn parse_timestamp(ts: &str) -> Option<DateTime<FixedOffset>> {
    crate::session::parse_any_timestamp(ts)
}

/// Resolve exact session identities before any lexical search runs.
pub(crate) fn direct_session_search(
    sessions: &[Session],
    query: &str,
    timeframe: Option<&str>,
) -> Option<Vec<SearchResult>> {
    let tf_cutoff: Option<DateTime<FixedOffset>> = timeframe.and_then(timeframe_cutoff_fixed);
    // The direct lookup honors --timeframe the way the index title entry
    // does: a message inside the window, else the session's own activity
    // time; a session outside the window falls through to content search.
    if is_uuid(query)
        && let Some(s) = sessions
            .iter()
            .find(|s| s.id.eq_ignore_ascii_case(query.trim()))
    {
        let (messages, _) = parse_session_recovering_timestamps(s, false);
        let in_window = |ts: &str| {
            tf_cutoff.is_none_or(|cutoff| parse_any_timestamp(ts).is_some_and(|t| t >= cutoff))
        };
        if let Some((ordinal, mut msg)) = messages
            .into_iter()
            .enumerate()
            .find(|(_, m)| in_window(&m.timestamp))
        {
            msg.final_score = 100.0;
            return Some(vec![SearchResult {
                session: s.clone(),
                message: msg,
                snippet: None,
                ordinal: Some(ordinal),
                additional_matches: Vec::new(),
            }]);
        }
        let activity = if s.modified.is_empty() {
            &s.created
        } else {
            &s.modified
        };
        if in_window(activity) {
            let stub = Message {
                uuid: String::new(),
                timestamp: activity.clone(),
                role: "user".into(),
                content: if !s.summary.is_empty() {
                    s.summary.clone()
                } else {
                    s.first_prompt.clone()
                },
                session_id: s.id.clone(),
                project_path: s.project.clone(),
                tool_uses: Vec::new(),
                files_referenced: Vec::new(),
                error_patterns: Vec::new(),
                relevance_score: 0.0,
                final_score: 100.0,
                history_output: false,
            };
            return Some(vec![SearchResult {
                session: s.clone(),
                message: stub,
                snippet: None,
                ordinal: None,
                additional_matches: Vec::new(),
            }]);
        }
        // Outside the window, content search may still find the id quoted in
        // another, recent conversation.
    }

    None
}

/// `--scope similar`: user messages (and titles) ranked by word overlap with
/// the query, then boosted and deduplicated. Callers resolve direct session
/// lookups first.
pub fn similar_search(
    sessions: &[Session],
    query: &str,
    limit: usize,
    timeframe: Option<&str>,
) -> Vec<SearchResult> {
    if limit == 0 {
        return Vec::new();
    }
    let tf_cutoff = timeframe.and_then(timeframe_cutoff_fixed);

    let boosts = semantic_boosts(query);
    let raw_words: Vec<String> = {
        let deduped: BTreeSet<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(String::from)
            .collect();
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
    let candidates: Vec<(Session, Message, String, Option<usize>)> = sessions
        .par_iter()
        .filter(|s| !s.file.is_empty() && std::path::Path::new(&s.file).exists())
        .flat_map(|s| {
            // Recovered times feed recency scoring, so recover them for every
            // search: the same hit must score the same with and without
            // --timeframe.
            let (messages, _) = parse_session_recovering_timestamps(s, false);
            let title = s.summary.clone();
            let mut messages: Vec<(Option<usize>, Message)> = messages
                .into_iter()
                .enumerate()
                .map(|(i, m)| (Some(i), m))
                .collect();
            if !title.is_empty() && !messages.iter().any(|(_, m)| m.content == title) {
                messages.insert(
                    0,
                    (
                        None,
                        Message {
                            uuid: "index-title".into(),
                            // The title entry carries the session's activity time
                            // for every source (Cursor's own updatedAtMs for CLI
                            // stores); message times come from the transcript.
                            timestamp: if s.modified.is_empty() {
                                s.created.clone()
                            } else {
                                s.modified.clone()
                            },
                            role: "user".into(),
                            content: title,
                            session_id: s.id.clone(),
                            project_path: s.project.clone(),
                            tool_uses: Vec::new(),
                            files_referenced: Vec::new(),
                            error_patterns: Vec::new(),
                            relevance_score: 0.0,
                            final_score: 0.0,
                            history_output: false,
                        },
                    ),
                );
            }
            let mut hits = Vec::new();
            for (ordinal, mut msg) in messages {
                let cl = msg.content_lower();
                if msg.history_output || is_noise(&cl) {
                    continue;
                }
                if msg.role != "user" {
                    continue;
                }

                if tf_cutoff.is_some() && msg.timestamp.is_empty() {
                    continue;
                }
                if let Some(cutoff) = tf_cutoff
                    && let Some(ts) = parse_timestamp(&msg.timestamp)
                    && ts < cutoff
                {
                    continue;
                }

                let sim = query_similarity(query, &msg.content);
                if sim < 0.25 {
                    continue;
                }
                msg.relevance_score = sim * 10.0;
                msg.session_id = s.id.clone();
                msg.project_path = s.project.clone();
                hits.push((s.clone(), msg, cl, ordinal));
            }
            hits
        })
        .collect();

    let mut results: Vec<(Session, Message, Option<usize>)> = candidates
        .into_iter()
        .map(|(s, mut msg, cl, ordinal)| {
            let mut score = msg.relevance_score;
            let match_count = query_terms
                .iter()
                .filter(|t| cl.contains(t.as_str()))
                .count();

            if match_count == 0 {
                score *= 0.1;
            }

            let mut boost = 1.0_f64;

            if match_count > 0 {
                boost *= (1.0 + 0.5 * match_count as f64).min(MAX_MULTIPLICATIVE_BOOST);
            }

            for (btype, bval) in &boosts {
                match *btype {
                    "error_resolution" if cl.contains("error") || cl.contains("exception") => {
                        boost *= bval
                    }
                    "solutions" if cl.contains("fix") || cl.contains("resolve") => boost *= bval,
                    "implementation" if cl.contains("implement") || cl.contains("create") => {
                        boost *= bval
                    }
                    "optimization"
                        if cl.contains("optimiz")
                            || cl.contains("performance")
                            || cl.contains("improve") =>
                    {
                        boost *= bval
                    }
                    "file_operations"
                        if cl.contains("file") || cl.contains("read") || cl.contains("write") =>
                    {
                        boost *= bval
                    }
                    "tool_usage" if cl.contains("tool") || !msg.tool_uses.is_empty() => {
                        boost *= bval
                    }
                    _ => {}
                }
            }

            boost *= importance_boost(&cl);

            if !msg.timestamp.is_empty() {
                let recency = recency_multiplier(&msg.timestamp);
                if recency >= 3.0 {
                    boost *= 1.5;
                } else if recency >= 2.0 {
                    boost *= 1.2;
                } else if recency >= 1.5 {
                    boost *= 1.1;
                }
            }

            if !msg.tool_uses.is_empty() {
                boost *= 1.3;
            }
            if !msg.files_referenced.is_empty() {
                boost *= 1.2;
            }
            if !msg.error_patterns.is_empty() {
                boost *= 1.4;
            }

            score *= boost.min(MAX_TOTAL_BOOST);
            msg.final_score = score;
            (s, msg, ordinal)
        })
        .collect();

    // Deduplicate
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut deduped: Vec<(Session, Message, Option<usize>)> = Vec::new();
    results.sort_by(|a, b| {
        b.1.final_score
            .partial_cmp(&a.1.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (s, msg, ordinal) in results {
        if msg.final_score <= 0.0 {
            continue;
        }
        let sig = content_signature(&msg.content, &msg.tool_uses, &msg.files_referenced);
        if let Some(idx) = seen.get(&sig) {
            if msg.final_score > deduped[*idx].1.final_score {
                deduped[*idx] = (s, msg, ordinal);
            }
        } else {
            seen.insert(sig, deduped.len());
            deduped.push((s, msg, ordinal));
        }
    }

    // Per-session cap
    let mut session_counts: HashMap<String, usize> = HashMap::new();
    let mut capped = Vec::new();
    for (s, m, o) in deduped {
        let count = session_counts.entry(s.id.clone()).or_insert(0);
        if *count < MAX_MATCHES_PER_SESSION {
            *count += 1;
            capped.push((s, m, o));
        }
    }

    // Quality gate with fallback (matches Python behavior)
    let quality: Vec<(Session, Message, Option<usize>)> = capped
        .iter()
        .filter(|(_, m, _)| {
            m.final_score >= 0.5 && (m.content.len() >= 40 || m.uuid == "index-title")
        })
        .cloned()
        .collect();
    let mut final_results = if quality.is_empty() { capped } else { quality };
    final_results.truncate(limit);
    final_results
        .into_iter()
        .map(|(session, message, ordinal)| SearchResult {
            session,
            message,
            snippet: None,
            ordinal,
            additional_matches: Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;

    fn make_session(
        id: &str,
        summary: &str,
        first_prompt: &str,
        project: &str,
        branch: &str,
    ) -> Session {
        Session {
            source: "claude".into(),
            id: id.into(),
            summary: summary.into(),
            first_prompt: first_prompt.into(),
            created: "2025-03-01T00:00:00".into(),
            modified: String::new(),
            date: "2025-03-01".into(),
            messages: 5,
            branch: branch.into(),
            project: project.into(),
            file: String::new(),
            is_sidechain: false,
            also_ide: false,
        }
    }

    #[test]
    fn max_total_boost_caps_combined_multiplier() {
        // Simulate worst case: all boosts active
        let mut boost = 1.0_f64;
        boost *= MAX_MULTIPLICATIVE_BOOST; // match count
        boost *= 3.0; // semantic: error_resolution
        boost *= 2.5; // importance_boost max
        boost *= 1.5; // recency
        boost *= 1.3; // tool_uses
        boost *= 1.2; // files_referenced
        boost *= 1.4; // error_patterns
        // Uncapped would be ~74x
        assert!(
            boost > MAX_TOTAL_BOOST,
            "uncapped boost should exceed limit"
        );
        let capped = boost.min(MAX_TOTAL_BOOST);
        assert_eq!(capped, MAX_TOTAL_BOOST, "capped boost should equal limit");
    }

    #[test]
    fn similar_search_includes_session_title_when_absent_from_transcript() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data = concat!(
            r#"{"type":"user","message":{"role":"user","content":"please look at the checkout workflow"},"timestamp":"2026-08-14T00:00:00Z","uuid":"u1"}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":"I will inspect the github actions file and suggest safer git fetch flags."},"timestamp":"2026-08-14T00:01:00Z","uuid":"u2"}"#,
        );
        std::fs::write(tmp.path(), data).unwrap();
        let mut s = make_session(
            "aaaa1111-bbbb-cccc-dddd-eeeeeeeeeeee",
            "review mergeability checks",
            "please look at the checkout workflow",
            "/home/alice/src/myapp",
            "main",
        );
        s.file = tmp.path().to_str().unwrap().to_string();
        // Session activity is recent while its messages stay dated 2026-08-14,
        // so the title-only hit depends on the session window, not messages.
        s.created = (chrono::Utc::now() - chrono::Duration::days(3))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        s.modified = s.created.clone();
        let results = similar_search(&[s.clone()], "review mergeability checks", 10, None);
        assert!(
            results
                .iter()
                .any(|r| r.message.content.contains("mergeability")),
            "expected title hit, got {:?}",
            results
                .iter()
                .map(|r| r.message.content.clone())
                .collect::<Vec<_>>()
        );
        let recent = similar_search(&[s.clone()], "review mergeability checks", 10, Some("30d"));
        assert!(
            recent
                .iter()
                .any(|r| r.message.content.contains("mergeability")),
            "title-only hit should survive a covering timeframe"
        );
    }

    #[test]
    fn parse_timeframe_duration_accepts_named_windows_and_nd() {
        assert_eq!(
            parse_timeframe_duration("today").unwrap(),
            chrono::Duration::try_days(1).unwrap()
        );
        assert_eq!(
            parse_timeframe_duration("7d").unwrap(),
            chrono::Duration::try_days(7).unwrap()
        );
        assert_eq!(
            parse_timeframe_duration("month").unwrap(),
            chrono::Duration::try_days(30).unwrap()
        );
    }

    #[test]
    fn parse_timeframe_duration_rejects_overflow_and_junk() {
        for tf in ["99999999999999d", "2weeks", "0d", "-5d", ""] {
            assert!(parse_timeframe_duration(tf).is_err(), "{tf}");
        }
    }
}
