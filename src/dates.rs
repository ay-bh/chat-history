use chrono::{Duration, Local, NaiveDate};
use regex::Regex;
use std::sync::LazyLock;

static AGO_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d+)\s+(day|week|month)s?\s+ago$").unwrap());
static LAST_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^last\s+(week|month)$").unwrap());

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_today() {
        let today = Local::now().date_naive();
        assert_eq!(parse_human_date("today"), Some(today));
        assert_eq!(parse_human_date("now"), Some(today));
    }

    #[test]
    fn parse_yesterday() {
        let yesterday = Local::now().date_naive() - Duration::days(1);
        assert_eq!(parse_human_date("yesterday"), Some(yesterday));
    }

    #[test]
    fn parse_days_ago() {
        let expected = Local::now().date_naive() - Duration::days(3);
        assert_eq!(parse_human_date("3 days ago"), Some(expected));
    }

    #[test]
    fn parse_weeks_ago() {
        let expected = Local::now().date_naive() - Duration::weeks(2);
        assert_eq!(parse_human_date("2 weeks ago"), Some(expected));
    }

    #[test]
    fn parse_months_ago() {
        let expected = Local::now().date_naive() - Duration::days(60);
        assert_eq!(parse_human_date("2 months ago"), Some(expected));
    }

    #[test]
    fn parse_last_week() {
        let expected = Local::now().date_naive() - Duration::weeks(1);
        assert_eq!(parse_human_date("last week"), Some(expected));
    }

    #[test]
    fn parse_last_month() {
        let expected = Local::now().date_naive() - Duration::days(30);
        assert_eq!(parse_human_date("last month"), Some(expected));
    }

    #[test]
    fn parse_iso_date() {
        assert_eq!(
            parse_human_date("2025-03-15"),
            Some(NaiveDate::from_ymd_opt(2025, 3, 15).unwrap())
        );
    }

    #[test]
    fn parse_invalid() {
        assert_eq!(parse_human_date("not a date"), None);
        assert_eq!(parse_human_date(""), None);
    }

    #[test]
    fn parse_case_insensitive() {
        let today = Local::now().date_naive();
        assert_eq!(parse_human_date("TODAY"), Some(today));
        assert_eq!(
            parse_human_date("Yesterday"),
            Some(today - Duration::days(1))
        );
    }

    #[test]
    fn parse_with_whitespace() {
        let today = Local::now().date_naive();
        assert_eq!(parse_human_date("  today  "), Some(today));
    }
}
