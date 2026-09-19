//! terms 模块的单元测试：术语增删查、冲突裁定、别名匹配与抽取降级。

use super::extract::ExtractionResponse;
use super::matching::{is_untranslated_source, same_target_format};
use super::{
    extract_terms, extract_terms_resilient, matches_text, PendingExtraction, Term, TermPolicy,
    TermStatus, TermStore,
};
use crate::llm::{CompletionOutput, MockClient, TranslationClient};
use async_trait::async_trait;
use rig_core::completion::Usage;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

fn store(name: &str) -> (TermStore, PathBuf) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "transitpls-terms-{name}-{}-{nonce}.db",
        std::process::id()
    ));
    (TermStore::open(&path).expect("store should open"), path)
}

fn term(source: &str, target: &str) -> Term {
    Term {
        source: source.to_string(),
        target: target.to_string(),
        reading: None,
        term_type: "person".to_string(),
        gender: None,
        aliases: Vec::new(),
        first_chapter: 0,
        note: None,
        status: TermStatus::Ok,
        policy: TermPolicy::Automatic,
        manual_target: None,
    }
}

fn json_term(source: &str, target: &str) -> serde_json::Value {
    serde_json::json!({
        "source": source,
        "target": target,
        "reading": null,
        "type": "person",
        "gender": null,
        "aliases": [],
        "note": null,
    })
}

#[test]
fn inserts_and_lists_terms() {
    let (store, path) = store("insert");
    assert_eq!(
        store
            .insert(&term("Alice", "爱丽丝"))
            .expect("term should insert"),
        TermStatus::Ok
    );
    let terms = store.list().expect("terms should list");
    assert_eq!(terms.len(), 1);
    assert_eq!(terms[0].source, "Alice");
    std::fs::remove_file(path).expect("database should be removed");
}

#[test]
fn records_conflicts_without_overwriting_the_current_target() {
    let (store, path) = store("conflict");
    store
        .insert(&term("Alice", "爱丽丝"))
        .expect("first term should insert");
    assert_eq!(
        store
            .insert(&term("Alice", "艾丽斯"))
            .expect("conflict should record"),
        TermStatus::Conflict
    );
    let terms = store.list().expect("terms should list");
    assert_eq!(terms[0].target, "爱丽丝");
    assert_eq!(terms[0].status, TermStatus::Conflict);
    assert_eq!(store.conflicts().expect("conflicts should list").len(), 2);
    std::fs::remove_file(path).expect("database should be removed");
}

#[test]
fn resolves_to_an_arbitrary_target_and_hides_settled_conflicts() {
    let (store, path) = store("resolve");
    store
        .insert(&term("Alice", "爱丽丝"))
        .expect("first term should insert");
    store
        .insert(&term("Alice", "艾丽斯"))
        .expect("conflict should record");
    store
        .resolve("Alice", "阿丽丝")
        .expect("term should resolve");
    let terms = store.list().expect("terms should list");
    assert_eq!(terms[0].target, "阿丽丝");
    assert_eq!(terms[0].status, TermStatus::Resolved);
    assert!(store.conflicts().expect("conflicts should list").is_empty());
    std::fs::remove_file(path).expect("database should be removed");
}

#[test]
fn deletes_term_with_evidence_conflicts_aliases_and_rules() {
    let (store, path) = store("delete");
    let mut alice = term("Alice", "爱丽丝");
    alice.aliases.push("Alicia".to_string());
    store.insert(&alice).unwrap();
    store.insert(&term("Alice", "艾丽斯")).unwrap();
    store.resolve("Alice", "阿丽丝").unwrap();
    let mut bob = term("Bob", "鲍勃");
    bob.aliases.push("Alicia".to_string());
    store.insert(&bob).unwrap();
    assert_eq!(store.alias_conflicts().unwrap().len(), 1);

    store.delete("Alice").unwrap();

    assert!(store
        .list()
        .unwrap()
        .iter()
        .all(|value| value.source != "Alice"));
    assert!(store.conflicts().unwrap().is_empty());
    assert!(store.conflict_details(true).unwrap().is_empty());
    assert!(store.alias_conflicts().unwrap().is_empty());
    assert!(store.delete("Alice").is_err());
    // 已删除的人工规则不应影响重新收录。
    assert_eq!(store.insert(&alice).unwrap(), TermStatus::Ok);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn manual_decision_stays_authoritative_while_new_evidence_is_kept() {
    let (store, path) = store("manual-authority");
    store.insert(&term("Alice", "爱丽丝")).unwrap();
    store
        .insert_with_evidence(&term("Alice", "艾丽斯"), "Alice arrived.", "艾丽斯到了。")
        .unwrap();
    store.resolve("Alice", "阿丽丝").unwrap();
    store
        .insert_with_evidence(&term("Alice", "爱莉丝"), "Alice left.", "爱莉丝离开了。")
        .unwrap();

    let stored = store.list().unwrap().remove(0);
    assert_eq!(stored.target, "阿丽丝");
    assert_eq!(stored.policy, TermPolicy::Fixed);
    assert_eq!(stored.manual_target.as_deref(), Some("阿丽丝"));
    let conflict = store.conflict_details(true).unwrap().remove(0);
    assert_eq!(conflict.unresolved_events, 0);
    assert_eq!(conflict.resolved_events, 2);
    assert!(conflict
        .candidates
        .iter()
        .any(|candidate| candidate.target == "爱莉丝"));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn policies_suppress_injection_and_restore_independent_conflicts() {
    let (store, path) = store("policies");
    store.insert(&term("Alice", "爱丽丝")).unwrap();
    store.insert(&term("Alice", "艾丽斯")).unwrap();
    store.insert(&term("Alice", "爱莉丝")).unwrap();
    assert_eq!(
        store.conflict_details(false).unwrap()[0].unresolved_events,
        2
    );

    store.set_policy("Alice", TermPolicy::NonFixed).unwrap();
    assert!(store.relevant("Alice arrived").unwrap().is_empty());
    store.insert(&term("Alice", "阿莉丝")).unwrap();
    assert!(store.conflict_details(false).unwrap().is_empty());

    store.undo_resolution("Alice").unwrap();
    assert_eq!(
        store.conflict_details(false).unwrap()[0].unresolved_events,
        3
    );
    store.set_policy("Alice", TermPolicy::Ignored).unwrap();
    store.insert(&term("Alice", "不会记录")).unwrap();
    assert!(!store.conflict_details(true).unwrap()[0]
        .candidates
        .iter()
        .any(|candidate| candidate.target == "不会记录"));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn migrates_legacy_conflicts_without_losing_candidates() {
    let (_, path) = store("legacy-path");
    std::fs::remove_file(&path).unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch(
        "CREATE TABLE terms (
                 source TEXT PRIMARY KEY, target TEXT NOT NULL, reading TEXT,
                 term_type TEXT NOT NULL, gender TEXT, aliases TEXT NOT NULL DEFAULT '[]',
                 first_chapter INTEGER NOT NULL, note TEXT,
                 status TEXT NOT NULL CHECK (status IN ('ok', 'conflict', 'resolved'))
             );
             CREATE TABLE term_candidates (
                 id INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT NOT NULL,
                 target TEXT NOT NULL, chapter INTEGER NOT NULL, UNIQUE(source, target)
             );
             INSERT INTO terms VALUES ('Alice', '爱丽丝', NULL, 'person', NULL, '[]', 0, NULL, 'conflict');
             INSERT INTO term_candidates (source, target, chapter) VALUES
                 ('Alice', '爱丽丝', 0), ('Alice', '艾丽斯', 1);"
    ).unwrap();
    drop(connection);

    let store = TermStore::open(&path).unwrap();
    let conflict = store.conflict_details(false).unwrap().remove(0);
    assert_eq!(conflict.unresolved_events, 1);
    assert_eq!(conflict.candidates.len(), 2);
    let connection = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn serializes_concurrent_writers_with_readers() {
    let (store, path) = store("concurrent-writers");
    let mode: String = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let mut workers = Vec::new();
    for worker in 0..8 {
        let path = path.clone();
        workers.push(std::thread::spawn(move || {
            let store = TermStore::open(&path).expect("store should open");
            for round in 0..10 {
                store
                    .insert(&term(&format!("worker-{worker}-{round}"), "译名"))
                    .expect("concurrent insert should not hit a locked database");
                store
                    .list()
                    .expect("concurrent read should not hit a locked database");
            }
        }));
    }
    for worker in workers {
        worker.join().expect("worker should finish");
    }
    assert_eq!(store.list().expect("terms should list").len(), 80);
    std::fs::remove_file(path).expect("database should be removed");
}

#[test]
fn filters_relevant_terms_by_source_and_alias() {
    let (store, path) = store("relevant");
    let mut alice = term("Alice", "爱丽丝");
    alice.aliases.push("ＡＬＩＣＩＡ".to_string());
    store.insert(&alice).expect("Alice should insert");
    store
        .insert(&term("王都", "王都"))
        .expect("CJK term should insert");
    store.insert(&term("cat", "猫")).expect("cat should insert");
    store
        .insert(&term("アリス", "爱丽丝"))
        .expect("Japanese term should insert");

    let relevant = store
        .relevant("Alicia entered the 王都. アリスは来た。Concatenate stayed behind.")
        .expect("relevant terms should filter");
    assert_eq!(relevant.len(), 3);
    assert!(relevant.iter().any(|term| term.source == "Alice"));
    assert!(relevant.iter().any(|term| term.source == "王都"));
    assert!(relevant.iter().any(|term| term.source == "アリス"));
    std::fs::remove_file(path).expect("database should be removed");
}

#[test]
fn records_ambiguous_aliases_and_does_not_inject_them() {
    let (store, path) = store("alias-conflict");
    let mut alice = term("Alice", "爱丽丝");
    alice.aliases.push("Captain".to_string());
    let mut bob = term("Bob", "鲍勃");
    bob.aliases.push("captain".to_string());
    store.insert(&alice).expect("Alice should insert");
    assert_eq!(
        store.insert(&bob).expect("Bob should insert"),
        TermStatus::Ok
    );

    assert_eq!(
        store.alias_conflicts().expect("aliases should list").len(),
        1
    );
    assert!(store
        .relevant("The CAPTAIN entered.")
        .expect("relevant terms should filter")
        .is_empty());
    assert!(store
        .list()
        .expect("terms should list")
        .iter()
        .all(|term| term.status == TermStatus::Ok));
    std::fs::remove_file(path).expect("database should be removed");
}

#[test]
fn persists_pending_extractions_until_completed() {
    let (store, path) = store("pending");
    let extraction = PendingExtraction {
        chapter_id: "chapter-1".to_string(),
        batch_key: "batch-1".to_string(),
        source_text: "Alice".to_string(),
        target_text: "爱丽丝".to_string(),
    };
    store
        .queue_extraction(&extraction)
        .expect("extraction should queue");
    assert_eq!(
        store
            .pending_extractions()
            .expect("pending extraction should list"),
        vec![extraction]
    );
    store
        .complete_extraction("chapter-1", "batch-1")
        .expect("extraction should complete");
    assert!(store
        .pending_extractions()
        .expect("pending extraction should list")
        .is_empty());
    std::fs::remove_file(path).expect("database should be removed");
}

#[tokio::test]
async fn extracts_stable_terms_with_mock_client() {
    let terms = extract_terms(&MockClient, "Alice arrived.", "爱丽丝到了。", &[], 2, 0)
        .await
        .expect("mock extraction should work");
    assert_eq!(terms.len(), 1);
    assert_eq!(terms[0].source, "Alice");
    assert_eq!(terms[0].first_chapter, 2);
}

#[test]
fn extraction_schema_constrains_term_types_and_wire_fields() {
    let schema = crate::schema::response_schema::<ExtractionResponse>().to_value();
    let properties = &schema["properties"]["terms"]["items"]["properties"];
    let types = properties["type"]["enum"]
        .as_array()
        .expect("type enum should be present");
    assert_eq!(types.len(), 7);
    assert!(properties.get("first_chapter").is_none());
    assert!(properties.get("status").is_none());
}

struct EmptyBatchClient;

#[async_trait]
impl TranslationClient for EmptyBatchClient {
    async fn complete(
        &self,
        _system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionOutput, String> {
        let request: serde_json::Value = serde_json::from_str(user_prompt).unwrap();
        let source = request["source"].as_str().unwrap();
        if source.contains('\n') {
            return Err(
                "ResponseError: Response contained no message or tool call (empty)".to_string(),
            );
        }
        let terms = match source {
            "Alice arrived." => vec![json_term("Alice", "爱丽丝")],
            "Alice met Bob." => vec![json_term("Alice", "爱丽丝"), json_term("Bob", "鲍勃")],
            _ => Vec::new(),
        };
        Ok(CompletionOutput {
            text: serde_json::json!({ "terms": terms }).to_string(),
            usage: Usage::default(),
        })
    }
}

#[tokio::test]
async fn empty_batch_response_splits_paragraphs_and_deduplicates_terms() {
    let terms = extract_terms_resilient(
        &EmptyBatchClient,
        "Alice arrived.\nAlice met Bob.",
        "爱丽丝到了。\n爱丽丝遇见了鲍勃。",
        &[],
        1,
        0,
    )
    .await
    .expect("paragraph fallback should recover the extraction");

    assert_eq!(
        terms
            .iter()
            .map(|term| term.source.as_str())
            .collect::<Vec<_>>(),
        vec!["Alice", "Bob"]
    );
}

struct AlwaysEmptyClient {
    failures: Arc<Mutex<Vec<serde_json::Value>>>,
}

#[async_trait]
impl TranslationClient for AlwaysEmptyClient {
    fn record_failure(&self, _stage: &str, details: serde_json::Value) -> Result<(), String> {
        self.failures.lock().unwrap().push(details);
        Ok(())
    }

    async fn complete(
        &self,
        _system_prompt: &str,
        _user_prompt: &str,
    ) -> Result<CompletionOutput, String> {
        Err("ResponseError: Response contained no message or tool call (empty)".to_string())
    }
}

#[tokio::test]
async fn empty_single_paragraph_is_recorded_without_blocking() {
    let failures = Arc::new(Mutex::new(Vec::new()));
    let terms = extract_terms_resilient(
        &AlwaysEmptyClient {
            failures: Arc::clone(&failures),
        },
        "No extractable response.",
        "没有可提取的响应。",
        &[],
        1,
        0,
    )
    .await
    .expect("a skipped paragraph should not fail translation");

    assert!(terms.is_empty());
    let failures = failures.lock().unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["kind"], "term_extraction_skipped");
}

struct ProseBatchClient;

#[async_trait]
impl TranslationClient for ProseBatchClient {
    async fn complete(
        &self,
        _system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionOutput, String> {
        let request: serde_json::Value = serde_json::from_str(user_prompt).unwrap();
        let source = request["source"].as_str().unwrap();
        if source.contains('\n') {
            return Ok(CompletionOutput {
                text: format!("Here are the requested terms, formatted below.\n{source}"),
                usage: Usage::default(),
            });
        }
        let terms = match source {
            "Alice arrived." => vec![json_term("Alice", "爱丽丝")],
            "Alice met Bob." => vec![json_term("Alice", "爱丽丝"), json_term("Bob", "鲍勃")],
            _ => Vec::new(),
        };
        Ok(CompletionOutput {
            text: serde_json::json!({ "terms": terms }).to_string(),
            usage: Usage::default(),
        })
    }
}

#[tokio::test]
async fn invalid_json_batch_splits_instead_of_failing() {
    let terms = extract_terms_resilient(
        &ProseBatchClient,
        "Alice arrived.\nAlice met Bob.",
        "爱丽丝到了。\n爱丽丝遇见了鲍勃。",
        &[],
        1,
        0,
    )
    .await
    .expect("invalid JSON should split and recover the extraction");

    assert_eq!(
        terms
            .iter()
            .map(|term| term.source.as_str())
            .collect::<Vec<_>>(),
        vec!["Alice", "Bob"]
    );
}

#[test]
fn matches_terms_in_legacy_text_with_flattened_ruby_readings() {
    assert!(matches_text(
        "『今日の 安 あ 達 だち さん』",
        "今日の安達さん"
    ));
    assert!(!matches_text("今日は安達さんに会った", "今日の安達さん"));
}

#[test]
fn ignores_format_only_target_differences() {
    let (store, path) = store("format-only");
    store.insert(&term("ピンポン", "乒乓")).unwrap();
    assert_eq!(
        store.insert(&term("ピンポン", "《乒乓》")).unwrap(),
        TermStatus::Ok
    );
    let stored = store.list().unwrap().remove(0);
    assert_eq!(stored.target, "乒乓");
    assert_eq!(stored.status, TermStatus::Ok);
    assert!(store.conflict_details(true).unwrap().is_empty());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn classifies_format_differences_and_identity_terms() {
    assert!(same_target_format("乒乓", "《乒乓》"));
    assert!(same_target_format("修科", "修科—"));
    assert!(!same_target_format("乒乓", "乒乓球"));
    assert!(is_untranslated_source("しまむら", "しまむら"));
    assert!(!is_untranslated_source("体育", "体育"));
    assert!(!is_untranslated_source("Alice", "爱丽丝"));
}

struct FixedTermClient(&'static str, &'static str);

#[async_trait]
impl TranslationClient for FixedTermClient {
    async fn complete(
        &self,
        _system_prompt: &str,
        _user_prompt: &str,
    ) -> Result<CompletionOutput, String> {
        Ok(CompletionOutput {
            text: serde_json::json!({ "terms": [json_term(self.0, self.1)] }).to_string(),
            usage: Usage::default(),
        })
    }
}

#[tokio::test]
async fn drops_extracted_terms_that_copy_the_kana_source_unchanged() {
    let terms = extract_terms(
        &FixedTermClient("しまむら", "しまむら"),
        "しまむらは来た。",
        "しまむらは来た。",
        &[],
        0,
        0,
    )
    .await
    .expect("identity extraction should parse");
    assert!(terms.is_empty());

    let terms = extract_terms(
        &FixedTermClient("体育", "体育"),
        "体育の時間。",
        "体育课的时间。",
        &[],
        0,
        0,
    )
    .await
    .expect("shared kanji term should parse");
    assert_eq!(terms.len(), 1);
}

struct PromptCaptureClient(Arc<Mutex<(String, String)>>);

#[async_trait]
impl TranslationClient for PromptCaptureClient {
    async fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionOutput, String> {
        *self.0.lock().unwrap() = (system_prompt.to_string(), user_prompt.to_string());
        Ok(CompletionOutput {
            text: serde_json::json!({ "terms": [json_term("Alice", "艾莉丝")] }).to_string(),
            usage: Usage::default(),
        })
    }
}

#[tokio::test]
async fn extraction_preserves_observed_target_and_records_glossary_conflict() {
    let prompt = Arc::new(Mutex::new((String::new(), String::new())));
    let (store, path) = store("observed-target");
    let mut alice = term("Alice", "爱丽丝");
    alice.aliases.push("Alicia".to_string());
    store.insert(&alice).unwrap();
    let terms = extract_terms(
        &PromptCaptureClient(Arc::clone(&prompt)),
        "Alicia arrived.",
        "艾莉丝到了。",
        &[alice],
        0,
        0,
    )
    .await
    .expect("observed extraction should parse");
    assert_eq!(terms[0].target, "艾莉丝");
    assert_eq!(
        store
            .insert_with_evidence(&terms[0], "Alicia arrived.", "艾莉丝到了。")
            .unwrap(),
        TermStatus::Conflict
    );
    assert_eq!(store.list().unwrap()[0].target, "爱丽丝");
    assert!(store
        .conflicts()
        .unwrap()
        .iter()
        .any(|term| term.target == "艾莉丝"));

    let (system, user) = &*prompt.lock().unwrap();
    assert!(system.contains("Report the target wording actually present"));
    assert!(!system.contains("reuse its target exactly"));
    assert!(system.contains("Extract only terminology"));
    assert!(system.contains("Exclude ordinary nouns"));
    assert!(system.contains("Return an empty array when nothing qualifies"));
    let value: serde_json::Value = serde_json::from_str(user).unwrap();
    assert_eq!(value["known_terms"][0]["source"], "Alice");
    assert_eq!(value["known_terms"][0]["target"], "爱丽丝");
    assert_eq!(value["known_terms"][0]["aliases"][0], "Alicia");
    assert_eq!(value["target"], "艾莉丝到了。");
    std::fs::remove_file(path).unwrap();
}
