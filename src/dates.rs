use chrono::{Duration, Local, NaiveDate};
use regex::Regex;
use std::sync::LazyLock;

static AGO_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d+)\s+(day|week|month)s?\s+ago$").unwrap());
static LAST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^last\s+(week|month)$").unwrap());

pub fn parse_human_date(s: &str) -> Option<NaiveDate> {
    let trimmed = s.trim();
    let lower = trimmed.to_lowercase();
    let today = Local::now().date_naive();

    match lower.as_str() {
        "today" | "now" => return Some(today),
        "yesterday" => return Some(today - Duration::days(1)),
        _ => {}
    }

    if let Some(caps) = AGO_RE.captures(&lower) {
        let n: i64 = caps[1].parse().ok()?;
        return Some(match &caps[2] {
            "day" => today - Duration::days(n),
            "week" => today - Duration::weeks(n),
            "month" => today - Duration::days(n * 30),
            _ => return None,
        });
    }

    if let Some(caps) = LAST_RE.captures(&lower) {
        return Some(match &caps[1] {
            "week" => today - Duration::weeks(1),
            "month" => today - Duration::days(30),
            _ => return None,
        });
    }

    NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").ok()
}
