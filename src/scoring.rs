use chrono::Local;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

pub const MAX_MULTIPLICATIVE_BOOST: f64 = 4.0;

static DIGIT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\d+"#).unwrap());
static QUOTE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"['"]"#).unwrap());
static WHITESPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\s+"#).unwrap());

pub fn is_uuid(query: &str) -> bool {
    let q = query.trim();
    if q.len() != 36 {
        return false;
    }
    let parts: Vec<&str> = q.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    let expected = [8, 4, 4, 4, 12];
    parts
        .iter()
        .zip(expected.iter())
        .all(|(p, &e)| p.len() == e && p.chars().all(|c| c.is_ascii_hexdigit()))
}

pub fn recency_multiplier(timestamp_str: &str) -> f64 {
    if timestamp_str.is_empty() {
        return 1.0;
    }
    let Some(ts) = crate::session::parse_any_timestamp(timestamp_str) else {
        return 1.0;
    };
    let now = Local::now().fixed_offset();
    let age = now.signed_duration_since(ts);
    if age.num_seconds() < 0 {
        return 3.0;
    }
    let days = age.num_days();
    if days < 1 {
        3.0
    } else if days < 7 {
        2.0
    } else if days < 30 {
        1.5
    } else {
        1.0
    }
}

fn has_query_word(query_lower: &str, word: &str) -> bool {
    query_lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .any(|w| w == word || w.starts_with(word))
}

pub fn semantic_boosts(query: &str) -> Vec<(&'static str, f64)> {
    let lq = query.to_lowercase();
    let mut boosts = Vec::new();
    if has_query_word(&lq, "error") {
        boosts.push(("error_resolution", 3.0));
    }
    if has_query_word(&lq, "implement") {
        boosts.push(("implementation", 2.5));
    }
    if has_query_word(&lq, "optimize") || has_query_word(&lq, "optimiz") {
        boosts.push(("optimization", 2.0));
    }
    if has_query_word(&lq, "fix") {
        boosts.push(("solutions", 2.8));
    }
    if has_query_word(&lq, "file") {
        boosts.push(("file_operations", 2.0));
    }
    if has_query_word(&lq, "tool") {
        boosts.push(("tool_usage", 2.2));
    }
    boosts
}

pub fn importance_boost(content_lower: &str) -> f64 {
    let mut max_b: f64 = 1.0;
    if [
        "decided to",
        "decision",
        "chose",
        "trade-off",
        "tradeoff",
        "rationale",
        "why we",
        "instead of",
        "opted for",
        "approach",
        "architecture",
        "design decision",
    ]
    .iter()
    .any(|p| content_lower.contains(p))
    {
        max_b = max_b.max(2.5);
    }
    if [
        "fixed",
        "bug",
        "gotcha",
        "workaround",
        "edge case",
        "issue",
        "problem",
        "broke",
        "breaking",
    ]
    .iter()
    .any(|p| content_lower.contains(p))
    {
        max_b = max_b.max(2.0);
    }
    if [
        "implemented",
        "shipped",
        "feature",
        "built",
        "created",
        "released",
    ]
    .iter()
    .any(|p| content_lower.contains(p))
    {
        max_b = max_b.max(1.5);
    }
    if [
        "learned",
        "discovered",
        "insight",
        "found out",
        "realize",
        "understanding",
        "now know",
    ]
    .iter()
    .any(|p| content_lower.contains(p))
    {
        max_b = max_b.max(1.3);
    }
    max_b
}

static TECHNICAL_SYNONYMS: LazyLock<HashMap<&'static str, &'static [&'static str]>> =
    LazyLock::new(|| {
        let mut m = HashMap::new();
        m.insert(
            "error",
            ["exception", "fail", "crash", "bug", "issue"].as_slice(),
        );
        m.insert("fix", &["resolve", "solve", "repair", "correct"]);
        m.insert("implement", &["create", "build", "develop", "add"]);
        m.insert("optimize", &["improve", "enhance", "performance"]);
        m.insert("debug", &["troubleshoot", "diagnose", "trace"]);
        m.insert("deploy", &["publish", "release", "launch"]);
        m.insert("auth", &["authentication", "login", "security"]);
        m.insert("api", &["endpoint", "service", "request"]);
        m
    });

static STEM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(ing|ed|s|ly|tion|ment)$").unwrap());

pub fn query_similarity(q1: &str, q2: &str) -> f64 {
    static STOP: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
        [
            "the", "and", "for", "that", "this", "with", "from", "have", "has", "how", "what",
            "when", "where", "why", "can", "could", "would", "should", "want", "need", "help",
            "please", "just", "like", "some",
        ]
        .into_iter()
        .collect()
    });

    let w1: Vec<String> = q1
        .to_lowercase()
        .split_whitespace()
        .filter(|w| w.len() > 2)
        .map(String::from)
        .collect();
    let w2: Vec<String> = q2
        .to_lowercase()
        .split_whitespace()
        .filter(|w| w.len() > 2)
        .map(String::from)
        .collect();
    if w1.is_empty() || w2.is_empty() {
        return 0.0;
    }

    let sig1: Vec<&str> = w1
        .iter()
        .filter(|w| w.len() >= 4 && !STOP.contains(w.as_str()))
        .map(|s| s.as_str())
        .collect();
    let sig2: Vec<&str> = w2
        .iter()
        .filter(|w| w.len() >= 4 && !STOP.contains(w.as_str()))
        .map(|s| s.as_str())
        .collect();

    let mut total_score: f64 = 0.0;
    let mut sig_matches = 0usize;
    let mut matched2: HashSet<usize> = HashSet::new();

    for word1 in &w1 {
        let mut best_match: f64 = 0.0;
        let mut best_idx: Option<usize> = None;
        let mut best_is_sig_pair = false;
        let is_sig1 = sig1.contains(&word1.as_str());

        for (j, word2) in w2.iter().enumerate() {
            if matched2.contains(&j) {
                continue;
            }
            let is_sig2 = sig2.contains(&word2.as_str());
            let mut match_score: f64 = 0.0;

            if word1 == word2 {
                match_score = 1.0;
            } else if word1.contains(word2.as_str()) || word2.contains(word1.as_str()) {
                let shorter = word1.len().min(word2.len());
                let longer = word1.len().max(word2.len());
                if shorter >= 5 && (shorter as f64 / longer as f64) >= 0.6 {
                    match_score = 0.8 * (shorter as f64 / longer as f64);
                }
            } else {
                for (key, syns) in TECHNICAL_SYNONYMS.iter() {
                    if (*key == word1 && syns.contains(&word2.as_str()))
                        || (*key == word2 && syns.contains(&word1.as_str()))
                        || (syns.contains(&word1.as_str()) && syns.contains(&word2.as_str()))
                    {
                        match_score = 0.7;
                        break;
                    }
                }
            }

            if match_score > best_match {
                best_match = match_score;
                best_idx = Some(j);
                best_is_sig_pair = is_sig1 && is_sig2 && match_score > 0.0;
            }
        }

        if let Some(idx) = best_idx {
            matched2.insert(idx);
            total_score += best_match;
            if best_is_sig_pair {
                sig_matches += 1;
            }
        }
    }

    if sig_matches < 1 && sig1.len() >= 2 && sig2.len() >= 2 {
        return 0.0;
    }

    let stem = |w: &str| -> String { STEM_RE.replace(w, "").to_string() };
    let s1: HashSet<String> = w1.iter().map(|w| stem(w)).collect();
    let s2: HashSet<String> = w2.iter().map(|w| stem(w)).collect();
    let stem_bonus = s1.intersection(&s2).count() as f64 / w1.len().max(w2.len()) as f64 * 0.3;

    let max_words = w1.len().max(w2.len()) as f64;
    (total_score / max_words + stem_bonus).min(1.0)
}

pub fn content_signature(text: &str, tools: &[String], files: &[String]) -> String {
    let mut norm = DIGIT_RE.replace_all(&text.to_lowercase(), "N").to_string();
    norm = QUOTE_RE.replace_all(&norm, "").to_string();
    norm = WHITESPACE_RE.replace_all(&norm, " ").to_string();
    let norm: String = norm.chars().take(200).collect();
    let t = {
        let mut sorted_tools: Vec<&str> = tools.iter().map(|s| s.as_str()).collect();
        sorted_tools.sort();
        sorted_tools.join("|")
    };
    let f = if files.is_empty() { "nofiles" } else { "files" };
    format!("{t}:{f}:{norm}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_uuid_valid() {
        assert!(is_uuid("550e8400-e29b-41d4-a716-446655440000"));
        assert!(is_uuid("  550e8400-e29b-41d4-a716-446655440000  "));
    }

    #[test]
    fn is_uuid_invalid() {
        assert!(!is_uuid("not-a-uuid"));
        assert!(!is_uuid(""));
        assert!(!is_uuid("550e8400-e29b-41d4-a716-44665544000")); // too short
        assert!(!is_uuid("550e8400-e29b-41d4-a716-4466554400000")); // too long
        assert!(!is_uuid("550e8400-e29b-41d4-a716-44665544000g")); // non-hex
        assert!(!is_uuid("550e8400e29b41d4a716446655440000")); // no dashes
    }

    #[test]
    fn query_similarity_identical() {
        let sim = query_similarity("implement authentication", "implement authentication");
        assert!(sim > 0.8, "Expected high similarity, got {sim}");
    }

    #[test]
    fn query_similarity_different() {
        let sim = query_similarity("implement authentication", "weather forecast today");
        assert!(sim < 0.3, "Expected low similarity, got {sim}");
    }

    #[test]
    fn query_similarity_synonyms() {
        let sim = query_similarity("fix error", "resolve bug");
        assert!(
            sim > 0.0,
            "Expected positive similarity for synonyms, got {sim}"
        );
    }

    #[test]
    fn query_similarity_empty() {
        assert_eq!(query_similarity("", "test"), 0.0);
        assert_eq!(query_similarity("test", ""), 0.0);
        assert_eq!(query_similarity("", ""), 0.0);
    }

    #[test]
    fn content_signature_deterministic() {
        let tools = vec!["Read".to_string()];
        let files = vec!["/src/main.rs".to_string()];
        let sig1 = content_signature("hello world 123", &tools, &files);
        let sig2 = content_signature("hello world 123", &tools, &files);
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn content_signature_normalizes_digits() {
        let sig = content_signature("error on line 42", &[], &[]);
        assert!(sig.contains('N'), "Digits should be replaced with N");
        assert!(!sig.contains("42"));
    }

    #[test]
    fn content_signature_differs_with_files() {
        let sig_no_files = content_signature("test", &[], &[]);
        let sig_with_files = content_signature("test", &[], &["main.rs".to_string()]);
        assert_ne!(
            sig_no_files, sig_with_files,
            "Files should change the signature for deduplication"
        );
    }

    #[test]
    fn content_signature_same_files_same_sig() {
        let files = vec!["main.rs".to_string()];
        let sig1 = content_signature("test", &[], &files);
        let sig2 = content_signature("same content different text", &[], &files);
        assert_ne!(
            sig1, sig2,
            "Different content text should produce different signatures even with same files"
        );
    }

    #[test]
    fn content_signature_sorted_tools() {
        let tools1 = vec!["Read".into(), "Edit".into()];
        let tools2 = vec!["Edit".into(), "Read".into()];
        let sig1 = content_signature("test", &tools1, &[]);
        let sig2 = content_signature("test", &tools2, &[]);
        assert_eq!(sig1, sig2, "Tool order should not matter");
    }

    #[test]
    fn semantic_boosts_error_query() {
        let boosts = semantic_boosts("fix error in code");
        let types: Vec<&str> = boosts.iter().map(|(t, _)| *t).collect();
        assert!(types.contains(&"error_resolution"));
        assert!(types.contains(&"solutions"));
    }

    #[test]
    fn semantic_boosts_implementation_query() {
        let boosts = semantic_boosts("implement file upload");
        let types: Vec<&str> = boosts.iter().map(|(t, _)| *t).collect();
        assert!(types.contains(&"implementation"));
        assert!(types.contains(&"file_operations"));
    }

    #[test]
    fn semantic_boosts_empty_for_generic() {
        let boosts = semantic_boosts("hello world");
        assert!(boosts.is_empty());
    }

    #[test]
    fn importance_boost_decision() {
        assert!(importance_boost("we decided to use react instead of vue") > 1.0);
    }

    #[test]
    fn importance_boost_bug_fix() {
        assert!(importance_boost("fixed the edge case bug in auth handler") > 1.0);
    }

    #[test]
    fn importance_boost_neutral() {
        assert_eq!(
            importance_boost("just a normal message nothing special here"),
            1.0
        );
    }

    #[test]
    fn recency_multiplier_empty() {
        assert_eq!(recency_multiplier(""), 1.0);
    }

    #[test]
    fn recency_multiplier_old_date() {
        assert_eq!(recency_multiplier("2020-01-01T00:00:00+00:00"), 1.0);
    }

    #[test]
    fn recency_multiplier_invalid() {
        assert_eq!(recency_multiplier("not-a-date"), 1.0);
    }

    #[test]
    fn has_query_word_exact() {
        assert!(has_query_word("fix error", "fix"));
        assert!(has_query_word("fix error", "error"));
    }

    #[test]
    fn has_query_word_prefix() {
        assert!(has_query_word("implementing auth", "implement"));
    }

    #[test]
    fn has_query_word_no_substring() {
        assert!(!has_query_word("complement this", "implement"));
        assert!(!has_query_word("profile settings", "file"));
        assert!(!has_query_word("terror attack", "error"));
    }

    #[test]
    fn has_query_word_with_punctuation() {
        assert!(has_query_word("(error)", "error"));
        assert!(has_query_word("\"error\"", "error"));
        assert!(has_query_word("`error`", "error"));
    }

    #[test]
    fn has_query_word_with_delimiters() {
        assert!(has_query_word("fix-error", "error"));
        assert!(has_query_word("fix-error", "fix"));
        assert!(has_query_word("error/fix", "fix"));
    }

    #[test]
    fn semantic_boosts_no_false_positive() {
        assert!(semantic_boosts("complement").is_empty());
        assert!(semantic_boosts("profile").is_empty());
        assert!(semantic_boosts("terror").is_empty());
    }

    #[test]
    fn semantic_boosts_still_matches_real_words() {
        assert!(!semantic_boosts("fix error").is_empty());
        assert!(!semantic_boosts("implementing auth").is_empty());
    }

    #[test]
    fn importance_boost_no_false_positive_new() {
        assert_eq!(importance_boost("the renewal process was slow"), 1.0);
        assert_eq!(importance_boost("reading the news about technology"), 1.0);
    }

    #[test]
    fn importance_boost_no_false_positive_added() {
        assert_eq!(importance_boost("the padded container looked correct"), 1.0);
    }

    #[test]
    fn importance_boost_still_triggers_on_real_words() {
        assert!(importance_boost("we implemented the authentication feature") > 1.0);
        assert!(importance_boost("successfully built the docker image") > 1.0);
    }
}
