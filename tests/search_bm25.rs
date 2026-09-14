use chat_history::search_index::{Bm25Backend, INDEX_FILENAME, SearchBackend, SearchRequest};
use chat_history::session::Session;
use serde_json::json;
use std::fs;
use tempfile::TempDir;

fn transcript(tmp: &TempDir, id: &str, texts: &[&str]) -> Session {
    let file = tmp.path().join(format!("{id}.jsonl"));
    write_messages(&file, texts);
    Session {
        id: id.into(),
        source: "claude".into(),
        file: file.to_string_lossy().into(),
        project: format!("/work/{id}"),
        created: "2026-01-01T00:00:00Z".into(),
        ..Session::default()
    }
}

fn write_messages(path: &std::path::Path, texts: &[&str]) {
    let data = texts
        .iter()
        .enumerate()
        .map(|(i, text)| {
            json!({
                "type": "user", "uuid": format!("m{i}"),
                "timestamp": "2026-01-01T00:00:00Z",
                "message": {"role": "user", "content": text}
            })
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(path, data).unwrap();
}

fn search(
    index: &mut dyn SearchBackend,
    sessions: &[Session],
    query: &str,
    limit: usize,
) -> Vec<chat_history::search::SearchResult> {
    index
        .search(&SearchRequest {
            sessions,
            query,
            scope: "all",
            limit,
            timeframe: None,
        })
        .unwrap()
}

#[test]
fn exact_acronyms_outrank_short_prefix_only_mentions() {
    let tmp = TempDir::new().unwrap();
    let corpus = vec![
        transcript(
            &tmp,
            "sqlite",
            &[
                "SQLite WAL checkpoints preserve committed database transactions while readers remain active.",
            ],
        ),
        transcript(&tmp, "city", &["Waltham headquarters"]),
        transcript(&tmp, "barrier", &["A wall"]),
        transcript(&tmp, "walk", &["Walking around"]),
    ];
    let mut index = Bm25Backend::open(None).unwrap();
    index.sync(&corpus, false).unwrap();
    assert_eq!(
        search(&mut index, &corpus, "WAL", 4)[0].session.id,
        "sqlite"
    );
    assert_eq!(
        search(&mut index, &corpus, "Walth", 4)[0].session.id,
        "city"
    );
}

#[test]
fn source_refresh_with_identical_messages_keeps_existing_postings() {
    let tmp = TempDir::new().unwrap();
    let corpus = vec![transcript(
        &tmp,
        "same",
        &["Existing checkpoint discussion"],
    )];
    let cache = tmp.path().join("cache");
    let mut index = Bm25Backend::open(Some(&cache)).unwrap();
    index.sync(&corpus, false).unwrap();
    let conn = rusqlite::Connection::open(cache.join(INDEX_FILENAME)).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_rewrite BEFORE DELETE ON messages
        BEGIN SELECT RAISE(ABORT, 'unchanged messages must retain postings'); END;",
    )
    .unwrap();
    // Source timestamps change, but parsing yields the same messages.
    write_messages(
        std::path::Path::new(&corpus[0].file),
        &["Existing checkpoint discussion"],
    );
    index.sync(&corpus, false).unwrap();
    assert_eq!(search(&mut index, &corpus, "checkpoint", 4).len(), 1);
    assert!(
        index.sync(&corpus, true).is_err(),
        "explicit rebuild must replace postings"
    );
    conn.execute("UPDATE sessions SET fingerprint = '[0,null]'", [])
        .unwrap();
    assert!(
        index.sync(&corpus, false).is_err(),
        "a changed extraction policy must replace postings"
    );
    conn.execute_batch("DROP TRIGGER reject_rewrite").unwrap();
    write_messages(
        std::path::Path::new(&corpus[0].file),
        &["Changed transaction discussion"],
    );
    index.sync(&corpus, false).unwrap();
    assert!(search(&mut index, &corpus, "checkpoint", 4).is_empty());
    assert_eq!(search(&mut index, &corpus, "transaction", 4).len(), 1);
}

#[test]
fn rare_terms_rank_above_generic_technology_mentions() {
    let tmp = TempDir::new().unwrap();
    let mut corpus = vec![transcript(
        &tmp,
        "answer",
        &["Investigate quasarprotocol handshake failures during client negotiation."],
    )];
    for i in 0..12 {
        corpus.push(transcript(
            &tmp,
            &format!("generic-{i}"),
            &[&format!(
                "Rust implementation and Rust compilation details for ordinary module {i}."
            )],
        ));
    }
    let mut index = Bm25Backend::open(None).unwrap();
    index.sync(&corpus, false).unwrap();
    let hits = search(&mut index, &corpus, "rust quasarprotocol", 10);
    assert_eq!(hits[0].session.id, "answer");
    assert!(hits[0].message.final_score > 0.0);
    assert!(hits.iter().all(|h| h.message.final_score.is_finite()));
}

#[test]
fn judged_query_fixture_compares_bm25_and_legacy() {
    let tmp = TempDir::new().unwrap();
    let cases = [
        (
            "protocol",
            "rust quasarprotocol",
            "Diagnose the quasarprotocol handshake during client negotiation.",
        ),
        (
            "filename",
            "src/token_cache.rs",
            "Update src/token_cache.rs to expire credentials after rotation.",
        ),
        (
            "errorcode",
            "ECONNRESET",
            "The upstream returned ECONNRESET after closing a keepalive socket.",
        ),
        (
            "acronym",
            "Go",
            "Our Go service limits concurrent workers with a buffered channel.",
        ),
        (
            "unicode",
            "cafe",
            "The café ordering service processes loyalty account updates.",
        ),
        (
            "identifier",
            "connection_pool",
            "The connection_pool blocks requests when all database leases are occupied.",
        ),
        (
            "multiword",
            "payment webhook",
            "Verify the payment webhook signature before acknowledging the event.",
        ),
        (
            "prefix",
            "authent",
            "Authentication checks the bearer token before routing a request.",
        ),
    ];
    let mut corpus: Vec<Session> = cases
        .iter()
        .map(|(id, _, text)| transcript(&tmp, id, &[*text]))
        .collect();
    for i in 0..12 {
        corpus.push(transcript(
            &tmp,
            &format!("generic-{i}"),
            &[&format!(
                "Rust compiler and Rust implementation details for ordinary module {i}."
            )],
        ));
    }
    let mut bm25 = Bm25Backend::open(None).unwrap();
    bm25.sync(&corpus, false).unwrap();
    let mut legacy = chat_history::search_index::LegacyBackend;
    let mut measures = Vec::new();
    for (name, backend) in [
        ("BM25", &mut bm25 as &mut dyn SearchBackend),
        ("legacy", &mut legacy),
    ] {
        let mut found = 0;
        let mut reciprocal_rank = 0.0;
        for (expected, query, _) in cases {
            let hits = search(backend, &corpus, query, 5);
            if let Some(rank) = hits.iter().position(|hit| hit.session.id == expected) {
                found += 1;
                reciprocal_rank += 1.0 / (rank + 1) as f64;
            }
            if name == "BM25" {
                assert_eq!(
                    hits.first().map(|hit| hit.session.id.as_str()),
                    Some(expected),
                    "{query}"
                );
            }
        }
        let mrr = reciprocal_rank / cases.len() as f64;
        eprintln!(
            "{name}: fixture Recall@5={found}/{}, MRR@5={mrr:.3}",
            cases.len()
        );
        measures.push(mrr);
    }
    assert!(measures[0] >= measures[1]);
}

#[test]
fn metadata_does_not_short_circuit_transcript_recall() {
    let tmp = TempDir::new().unwrap();
    let mut title = transcript(
        &tmp,
        "title",
        &["Discuss the next release checklist and ownership."],
    );
    title.summary = "authentication".into();
    let content = transcript(
        &tmp,
        "body",
        &["Authentication uses an expiring token checked by the gateway."],
    );
    let corpus = vec![title, content];
    let mut index = Bm25Backend::open(None).unwrap();
    index.sync(&corpus, false).unwrap();
    let hits = search(&mut index, &corpus, "authentication", 10);
    assert!(hits.iter().any(|h| h.session.id == "title"));
    assert!(hits.iter().any(|h| h.session.id == "body"));
}

#[test]
fn unicode_identifiers_prefixes_short_queries_and_literal_syntax() {
    let tmp = TempDir::new().unwrap();
    let corpus = vec![transcript(
        &tmp,
        "code",
        &[
            "Implement authentication using src/parser.rs and connection_pool for café clients.",
            "The C and Go implementations both support 日本語 names in requests.",
        ],
    )];
    let mut index = Bm25Backend::open(None).unwrap();
    index.sync(&corpus, false).unwrap();
    for query in [
        "auth",
        "src/parser.rs",
        "connection_pool",
        "cafe",
        "日本語",
        "C",
        "Go",
        "\"auth\" OR * :",
    ] {
        assert!(
            !search(&mut index, &corpus, query, 10).is_empty(),
            "{query}"
        );
    }
    for query in ["", "   ", "*** : \"()", "unfindabletoken"] {
        assert!(search(&mut index, &corpus, query, 10).is_empty(), "{query}");
    }
    let excessive = (0..65)
        .map(|i| format!("term{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        index
            .search(&SearchRequest {
                sessions: &corpus,
                query: &excessive,
                scope: "all",
                limit: 10,
                timeframe: None
            })
            .is_err()
    );
}

#[test]
fn overlapping_passages_collapse_to_original_messages_and_fill_limit() {
    let tmp = TempDir::new().unwrap();
    let long = format!(
        "{} boundaryneedle {}",
        "日".repeat(1590),
        "tail ".repeat(1500)
    );
    let busy: Vec<String> = (0..12)
        .map(|i| {
            format!(
                "boundaryneedle discussion for unique subsection {} with useful details",
                char::from(b'a' + i)
            )
        })
        .collect();
    let mut texts: Vec<&str> = busy.iter().map(String::as_str).collect();
    texts.push(&long);
    let corpus = vec![
        transcript(&tmp, "busy", &texts),
        transcript(
            &tmp,
            "other",
            &["A separate boundaryneedle reference with a distinct explanation."],
        ),
    ];
    let mut index = Bm25Backend::open(None).unwrap();
    index.sync(&corpus, false).unwrap();
    let hits = search(&mut index, &corpus, "boundaryneedle", 4);
    assert_eq!(hits.len(), 4);
    assert_eq!(hits.iter().filter(|h| h.session.id == "busy").count(), 3);
    assert!(hits.iter().any(|h| h.session.id == "other"));
    let hits = search(&mut index, &corpus, "tail", 10);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].message.content, long);
}

#[test]
fn filtering_happens_before_limit_and_does_not_change_scores() {
    let tmp = TempDir::new().unwrap();
    let corpus = vec![
        transcript(
            &tmp,
            "excluded",
            &["Connection pooling connection pooling connection pooling."],
        ),
        transcript(
            &tmp,
            "included",
            &["Configure connection pooling for the payment gateway."],
        ),
    ];
    let mut index = Bm25Backend::open(None).unwrap();
    index.sync(&corpus, false).unwrap();
    let all = search(&mut index, &corpus, "pooling", 10);
    let filtered = search(&mut index, &corpus[1..], "pooling", 1);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].session.id, "included");
    assert_eq!(
        filtered[0].message.final_score,
        all.iter()
            .find(|h| h.session.id == "included")
            .unwrap()
            .message
            .final_score
    );
    assert!(search(&mut index, &corpus, "pooling", 0).is_empty());
}

#[test]
fn unknown_prompt_timestamps_are_not_replaced_by_session_activity() {
    let tmp = TempDir::new().unwrap();
    let mut s = transcript(&tmp, "untimed", &[]);
    s.modified = chrono::Utc::now().to_rfc3339();
    s.summary = "Current titlemarker".into();
    s.first_prompt = "An unknown promptneedle message from a past conversation".into();
    fs::write(
        &s.file,
        json!({"type":"user", "message":{"role":"user", "content":s.first_prompt}}).to_string(),
    )
    .unwrap();
    let corpus = vec![s];
    let mut index = Bm25Backend::open(None).unwrap();
    index.sync(&corpus, false).unwrap();
    let mut request = SearchRequest {
        sessions: &corpus,
        query: "promptneedle",
        scope: "all",
        limit: 10,
        timeframe: Some("today"),
    };
    assert!(index.search(&request).unwrap().is_empty());
    request.query = "titlemarker";
    assert_eq!(index.search(&request).unwrap().len(), 1);
}

#[test]
fn scopes_keep_only_matching_message_kinds() {
    let tmp = TempDir::new().unwrap();
    let s = transcript(&tmp, "scopes", &[]);
    fs::write(&s.file, [
        json!({"type":"user","message":{"role":"user","content":"Investigate needlemarker in the regular conversation."}}),
        json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/src/needlemarker.rs"}}]}}),
        json!({"type":"assistant","message":{"role":"assistant","content":"An error in needlemarker requires investigating the exception."}}),
    ].iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")).unwrap();
    let corpus = vec![s];
    let mut index = Bm25Backend::open(None).unwrap();
    index.sync(&corpus, false).unwrap();
    for scope in ["errors", "tools", "files"] {
        let hits = index
            .search(&SearchRequest {
                sessions: &corpus,
                query: "needlemarker",
                scope,
                limit: 10,
                timeframe: None,
            })
            .unwrap();
        assert!(!hits.is_empty(), "{scope}");
        for hit in hits {
            match scope {
                "errors" => assert!(
                    hit.message.content.contains("error") || !hit.message.error_patterns.is_empty()
                ),
                "tools" => assert!(!hit.message.tool_uses.is_empty()),
                "files" => assert!(!hit.message.files_referenced.is_empty()),
                _ => unreachable!(),
            }
        }
    }
}

#[test]
fn persistent_index_refreshes_edits_metadata_deletions_and_rebuilds() {
    let tmp = TempDir::new().unwrap();
    let mut corpus = vec![transcript(
        &tmp,
        "source",
        &["Original uniquebefore message for cache verification."],
    )];
    // Match the shared metadata cache's conservative two-second racy window.
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let dir = tmp.path().join("index");
    let mut index = Bm25Backend::open(Some(&dir)).unwrap();
    assert_eq!(index.sync(&corpus, false).unwrap().updated, 1);
    drop(index);
    let mut index = Bm25Backend::open(Some(&dir)).unwrap();
    assert_eq!(index.sync(&corpus, false).unwrap().unchanged, 1);
    let before = search(&mut index, &corpus, "uniquebefore", 10)[0]
        .message
        .final_score;
    let mut ephemeral = Bm25Backend::open(None).unwrap();
    ephemeral.sync(&corpus, false).unwrap();
    assert_eq!(
        before,
        search(&mut ephemeral, &corpus, "uniquebefore", 10)[0]
            .message
            .final_score
    );
    assert_eq!(index.sync(&corpus, true).unwrap().updated, 1);
    write_messages(
        std::path::Path::new(&corpus[0].file),
        &["Updated uniqueafter message for cache verification."],
    );
    corpus[0].summary = "Renamed uniquetitle".into();
    assert_eq!(index.sync(&corpus, false).unwrap().updated, 1);
    assert!(search(&mut index, &corpus, "uniquebefore", 10).is_empty());
    assert!(!search(&mut index, &corpus, "uniqueafter", 10).is_empty());
    assert!(!search(&mut index, &corpus, "uniquetitle", 10).is_empty());
    fs::remove_file(&corpus[0].file).unwrap();
    assert_eq!(index.sync(&[], false).unwrap().removed, 1);
    assert!(search(&mut index, &corpus, "uniqueafter", 10).is_empty());
    let conn = rusqlite::Connection::open(dir.join(INDEX_FILENAME)).unwrap();
    conn.execute(
        "INSERT INTO passages_fts(passages_fts) VALUES ('integrity-check')",
        [],
    )
    .unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(dir.join(INDEX_FILENAME))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

fn command(tmp: &TempDir) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("chat-history").unwrap();
    cmd.env("HOME", tmp.path())
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .env_remove("CODEX_HOME")
        .env_remove("CHAT_HISTORY_SEARCH_ENGINE")
        .env_remove("CHAT_HISTORY_NO_CACHE")
        .env_remove("CHAT_HISTORY_CACHE_DIR");
    cmd
}

fn cli_fixture(tmp: &TempDir) {
    let dir = tmp.path().join("projects/demo");
    fs::create_dir_all(&dir).unwrap();
    write_messages(
        &dir.join("11111111-2222-3333-4444-555555555555.jsonl"),
        &["Investigate uniquecli authentication failures during login."],
    );
}

#[test]
fn cli_defaults_to_bm25_and_supports_engine_environment_and_no_cache() {
    let tmp = TempDir::new().unwrap();
    cli_fixture(&tmp);
    let out = command(&tmp)
        .args(["search", "uniquecli", "--json", "--no-cache"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(json["count"].as_u64().unwrap() > 0);
    assert!(json["results"][0]["score"].as_f64().unwrap() > 0.0);
    assert!(json["results"][0]["role"].is_string());
    assert!(
        !tmp.path()
            .join(".chat-history/cache")
            .join(INDEX_FILENAME)
            .exists()
    );
    command(&tmp)
        .env("CHAT_HISTORY_SEARCH_ENGINE", "legacy")
        .args(["search", "uniquecli", "--engine", "bm25", "--json"])
        .assert()
        .success();
    assert!(
        tmp.path()
            .join(".chat-history/cache")
            .join(INDEX_FILENAME)
            .exists()
    );
    command(&tmp)
        .args(["search", "uniquecli", "--engine", "unknown"])
        .assert()
        .code(2);
}

#[test]
fn corrupt_cache_falls_back_without_changing_search_results() {
    let tmp = TempDir::new().unwrap();
    cli_fixture(&tmp);
    let dir = tmp.path().join("index");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join(INDEX_FILENAME), "not a database").unwrap();
    let broken = command(&tmp)
        .env("CHAT_HISTORY_CACHE_DIR", &dir)
        .args(["search", "uniquecli", "--json"])
        .output()
        .unwrap();
    let memory = command(&tmp)
        .args(["search", "uniquecli", "--json", "--no-cache"])
        .output()
        .unwrap();
    assert!(
        broken.status.success(),
        "{}",
        String::from_utf8_lossy(&broken.stderr)
    );
    assert_eq!(broken.stdout, memory.stdout);
    assert!(String::from_utf8_lossy(&broken.stderr).contains("in-memory BM25"));
}

#[test]
fn sqlite_wal_changes_refresh_messages_without_main_database_changes() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("state.vscdb");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
        CREATE TABLE composerHeaders(composerId TEXT PRIMARY KEY, value TEXT);
        CREATE TABLE cursorDiskKV(key TEXT PRIMARY KEY, value TEXT);",
    )
    .unwrap();
    let id = "wal-session";
    conn.execute("INSERT INTO composerHeaders VALUES (?1, '{}')", [id])
        .unwrap();
    conn.execute("INSERT INTO cursorDiskKV VALUES (?1, ?2)", rusqlite::params![format!("bubbleId:{id}:one"),
        json!({"type":1,"text":"Original walbefore message about authentication.","createdAt":"2026-01-01T00:00:00Z"}).to_string()]).unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let corpus = vec![Session {
        source: "cursor-ide".into(),
        id: id.into(),
        file: path.to_string_lossy().into(),
        ..Session::default()
    }];
    let mut index = Bm25Backend::open(None).unwrap();
    index.sync(&corpus, false).unwrap();
    assert_eq!(index.sync(&corpus, false).unwrap().unchanged, 1);
    assert!(!search(&mut index, &corpus, "walbefore", 10).is_empty());
    let before = fs::metadata(&path).unwrap().modified().unwrap();
    conn.execute("UPDATE cursorDiskKV SET value=?1", [json!({"type":1,"text":"Changed walafter message about authentication.","createdAt":"2026-01-01T00:00:00Z"}).to_string()]).unwrap();
    assert_eq!(before, fs::metadata(&path).unwrap().modified().unwrap());
    assert_eq!(index.sync(&corpus, false).unwrap().updated, 1);
    assert!(search(&mut index, &corpus, "walbefore", 10).is_empty());
    assert!(!search(&mut index, &corpus, "walafter", 10).is_empty());
}

#[test]
fn locked_cache_uses_ephemeral_bm25_and_preserves_json() {
    let tmp = TempDir::new().unwrap();
    cli_fixture(&tmp);
    command(&tmp)
        .args(["search", "uniquecli", "--json"])
        .assert()
        .success();
    let conn =
        rusqlite::Connection::open(tmp.path().join(".chat-history/cache").join(INDEX_FILENAME))
            .unwrap();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    // The recent source needs a refresh, so a held write lock must trigger fallback.
    let output = command(&tmp)
        .args(["search", "uniquecli", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("in-memory BM25"));
    assert!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["count"]
            .as_u64()
            .unwrap()
            > 0
    );
}

#[test]
fn uuid_lookup_and_quoted_uuid_fallback_work_with_both_engines() {
    let tmp = TempDir::new().unwrap();
    let uuid = "12345678-abcd-4321-8888-123456789abc";
    let corpus = vec![
        transcript(
            &tmp,
            uuid,
            &["A directly addressable session about deployment."],
        ),
        transcript(
            &tmp,
            "mention",
            &[&format!(
                "We discussed session {uuid} in yesterday's retrospective."
            )],
        ),
    ];
    let mut index = Bm25Backend::open(None).unwrap();
    index.sync(&corpus, false).unwrap();
    let mut legacy = chat_history::search_index::LegacyBackend;
    for backend in [&mut index as &mut dyn SearchBackend, &mut legacy] {
        assert_eq!(
            search(backend, &corpus, &uuid.to_uppercase(), 10)[0]
                .session
                .id,
            uuid
        );
        assert_eq!(
            search(backend, &corpus[1..], uuid, 10)[0].session.id,
            "mention"
        );
        assert!(search(backend, &corpus, uuid, 0).is_empty());
    }
}

#[test]
fn concurrent_profiles_search_their_own_consistent_snapshot() {
    let tmp = TempDir::new().unwrap();
    let directory = tmp.path().join("index");
    drop(Bm25Backend::open(Some(&directory)).unwrap());
    let a = vec![transcript(
        &tmp,
        "profile-a",
        &["Profilealpha conversation about needleconcurrent behavior."],
    )];
    let b = vec![transcript(
        &tmp,
        "profile-b",
        &["Profilebeta conversation about needleconcurrent behavior."],
    )];
    std::thread::scope(|scope| {
        for corpus in [&a, &b] {
            let directory = &directory;
            scope.spawn(move || {
                for _ in 0..8 {
                    let hits = chat_history::search_index::search_corpus(
                        corpus,
                        &SearchRequest {
                            sessions: corpus,
                            query: "needleconcurrent",
                            scope: "all",
                            limit: 10,
                            timeframe: None,
                        },
                        Some(directory),
                        false,
                    )
                    .unwrap();
                    assert_eq!(hits.len(), 1);
                    assert_eq!(hits[0].session.id, corpus[0].id);
                }
            });
        }
    });
}

#[test]
fn numeric_error_codes_are_distinct_search_results() {
    let tmp = TempDir::new().unwrap();
    let corpus = vec![transcript(
        &tmp,
        "errors",
        &[
            "Service returned error E100 during credential validation.",
            "Service returned error E200 during credential validation.",
            "Service returned error E100 during credential validation.",
        ],
    )];
    let mut index = Bm25Backend::open(None).unwrap();
    index.sync(&corpus, false).unwrap();
    let hits = search(&mut index, &corpus, "credential", 10);
    assert_eq!(hits.len(), 2);
}

#[test]
fn per_session_cap_applies_across_transcript_copies() {
    let tmp = TempDir::new().unwrap();
    let mut a = transcript(
        &tmp,
        "copy-a",
        &[
            "copyneedle alpha workstream implements the gateway service.",
            "copyneedle beta workstream checks deployment credentials.",
            "copyneedle gamma workstream reviews the release pipeline.",
        ],
    );
    let mut b = transcript(
        &tmp,
        "copy-b",
        &[
            "copyneedle delta workstream fixes the request validator.",
            "copyneedle epsilon workstream adds new integration checks.",
            "copyneedle zeta workstream rotates the signing certificates.",
        ],
    );
    a.id = "same-session".into();
    b.id = a.id.clone();
    let corpus = vec![a, b];
    let mut index = Bm25Backend::open(None).unwrap();
    index.sync(&corpus, false).unwrap();
    assert_eq!(search(&mut index, &corpus, "copyneedle", 10).len(), 3);
}

#[test]
fn prompt_preview_must_match_the_message_whose_timestamp_it_uses() {
    let tmp = TempDir::new().unwrap();
    let mut s = transcript(&tmp, "preview", &[]);
    s.first_prompt = "Original promptneedle discussed during an older conversation.".into();
    fs::write(&s.file, [
        json!({"type":"user", "timestamp":chrono::Utc::now().to_rfc3339(), "message":{"role":"user","content":"An unrelated warmup message before the stored conversation."}}),
        json!({"type":"user", "timestamp":"2020-01-01T00:00:00Z", "message":{"role":"user","content":s.first_prompt}}),
    ].iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")).unwrap();
    let corpus = vec![s];
    let mut index = Bm25Backend::open(None).unwrap();
    index.sync(&corpus, false).unwrap();
    let hits = index
        .search(&SearchRequest {
            sessions: &corpus,
            query: "promptneedle",
            scope: "all",
            limit: 10,
            timeframe: Some("today"),
        })
        .unwrap();
    assert!(hits.is_empty());
}

#[test]
fn passage_boundaries_do_not_create_fake_word_prefixes() {
    let tmp = TempDir::new().unwrap();
    let content = format!(
        "{}headsuffixneedle{}",
        "pad ".repeat(349),
        " extra".repeat(100)
    );
    let corpus = vec![transcript(&tmp, "boundaries", &[&content])];
    let mut index = Bm25Backend::open(None).unwrap();
    index.sync(&corpus, false).unwrap();
    assert!(search(&mut index, &corpus, "suffixneedle", 10).is_empty());
    assert_eq!(search(&mut index, &corpus, "headsuffixneedle", 10).len(), 1);
}

#[test]
fn query_analysis_preserves_unicode_and_identifier_structure() {
    let tmp = TempDir::new().unwrap();
    let corpus = vec![
        transcript(
            &tmp,
            "unicode-target",
            &["The cafeteria service handles lunch orders and payments."],
        ),
        transcript(
            &tmp,
            "unicode-decoy",
            &["The cafe repairs its espresso machines every morning."],
        ),
        transcript(
            &tmp,
            "path-target",
            &["Inspect src/token_cache.rs to debug credential expiration."],
        ),
        transcript(
            &tmp,
            "path-decoy",
            &["Inspect src/router.rs to debug downstream request dispatch."],
        ),
    ];
    let mut index = Bm25Backend::open(None).unwrap();
    index.sync(&corpus, false).unwrap();
    for (query, expected) in [
        ("cafe\u{301}te\u{301}ria", "unicode-target"),
        ("src/token_cache.rs", "path-target"),
    ] {
        let hits = search(&mut index, &corpus, query, 10);
        assert_eq!(hits.len(), 1, "{query}");
        assert_eq!(hits[0].session.id, expected);
    }
}

#[test]
fn cache_commit_failure_does_not_discard_valid_results() {
    let tmp = TempDir::new().unwrap();
    cli_fixture(&tmp);
    let dir = tmp.path().join("index");
    drop(Bm25Backend::open(Some(&dir)).unwrap());
    let conn = rusqlite::Connection::open(dir.join(INDEX_FILENAME)).unwrap();
    // Deterministically fail COMMIT after synchronization and retrieval succeed.
    conn.execute_batch(
        "CREATE TABLE commit_guard (
        missing_key TEXT REFERENCES sessions(key) DEFERRABLE INITIALLY DEFERRED);
        CREATE TRIGGER fail_commit AFTER INSERT ON messages BEGIN
          INSERT INTO commit_guard VALUES ('missing');
        END;",
    )
    .unwrap();
    let output = command(&tmp)
        .env("CHAT_HISTORY_CACHE_DIR", &dir)
        .args(["search", "uniquecli", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["count"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("could not save search cache"));
    assert_eq!(
        conn.query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn missing_fts_asset_is_rebuilt_from_existing_message_rows() {
    let tmp = TempDir::new().unwrap();
    let corpus = vec![transcript(
        &tmp,
        "repair",
        &["Repairneedle verifies the derived full text index."],
    )];
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let dir = tmp.path().join("index");
    let mut index = Bm25Backend::open(Some(&dir)).unwrap();
    index.sync(&corpus, false).unwrap();
    drop(index);
    let conn = rusqlite::Connection::open(dir.join(INDEX_FILENAME)).unwrap();
    conn.execute_batch("DROP TABLE passages_fts").unwrap();
    drop(conn);
    let mut index = Bm25Backend::open(Some(&dir)).unwrap();
    assert_eq!(index.sync(&corpus, false).unwrap().unchanged, 1);
    assert_eq!(search(&mut index, &corpus, "repairneedle", 10).len(), 1);
    let conn = rusqlite::Connection::open(dir.join(INDEX_FILENAME)).unwrap();
    conn.execute(
        "INSERT INTO passages_fts(passages_fts, rank) VALUES ('integrity-check', 1)",
        [],
    )
    .unwrap();
}

#[test]
#[ignore = "Synthetic release-mode performance comparison; run with --ignored --nocapture"]
fn benchmark_cold_warm_and_legacy() {
    use std::time::Instant;
    let tmp = TempDir::new().unwrap();
    let mut corpus = Vec::new();
    for session in 0..250 {
        let messages: Vec<String> = (0..20)
            .map(|message| {
                format!(
                    "Investigate component{session} operation{message}: {} marker{session}.",
                    "The request handler validates credentials and opens a connection. ".repeat(8)
                )
            })
            .collect();
        corpus.push(transcript(
            &tmp,
            &format!("s{session}"),
            &messages.iter().map(String::as_str).collect::<Vec<_>>(),
        ));
    }
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let directory = tmp.path().join("index");
    let start = Instant::now();
    let mut index = Bm25Backend::open(Some(&directory)).unwrap();
    index.sync(&corpus, false).unwrap();
    let cold = start.elapsed();
    drop(index);
    let start = Instant::now();
    let mut index = Bm25Backend::open(Some(&directory)).unwrap();
    let stats = index.sync(&corpus, false).unwrap();
    assert_eq!(stats.unchanged, corpus.len());
    let warm = start.elapsed();
    let start = Instant::now();
    assert!(!search(&mut index, &corpus, "marker137", 15).is_empty());
    let query = start.elapsed();
    let start = Instant::now();
    assert!(
        !search(
            &mut chat_history::search_index::LegacyBackend,
            &corpus,
            "marker137",
            15
        )
        .is_empty()
    );
    let legacy = start.elapsed();
    eprintln!(
        "250 sessions / 5000 messages: cold={cold:?}; warm open+sync={warm:?}; BM25 query={query:?}; legacy deep query={legacy:?}"
    );
}
