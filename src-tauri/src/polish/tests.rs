//! polish 模块的单元测试：批次规划、并发控制、顺序写回与中断恢复。

use super::plan::{drafts_digest, plan_batches, terms_digest};
use super::run::run_round;
use super::store::{read_summary, round_path};
use super::*;
use crate::analysis::BookAnalysis;
use crate::config::AppConfig;
use crate::llm::{CompletionOutput, TranslationClient};
use crate::model::{
    Document, DocumentMetadata, ItemStatus, PolishStatus, ProjectState, Segment, SegmentKind,
};
use crate::state;
use async_trait::async_trait;
use rig_core::completion::Usage;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn temp_dir() -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "transitpls-polish-{}-{nonce}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).expect("temp directory should be created");
    path
}

fn document(chapters: usize, segments: usize) -> Document {
    Document {
        metadata: DocumentMetadata {
            title: "Book".to_string(),
            source_language: "en".to_string(),
            target_language: "zh-CN".to_string(),
            source_format: "txt".to_string(),
        },
        chapters: (0..chapters)
            .map(|chapter| Chapter {
                id: format!("chapter-{chapter}"),
                title: format!("Chapter {chapter}"),
                target_title: Some(format!("第 {chapter} 章")),
                status: ItemStatus::Translated,
                meta: serde_json::json!({ "source_digest": format!("digest {chapter}") }),
                segments: (0..segments)
                    .map(|segment| Segment {
                        id: format!("c{chapter}-s{segment}"),
                        ordinal: segment,
                        source: format!("source {chapter}-{segment}"),
                        target: Some(format!("draft {chapter}-{segment}")),
                        target_before_polish: Some(format!("draft {chapter}-{segment}")),
                        polish_status: Some(PolishStatus::Pending),
                        kind: SegmentKind::Paragraph,
                        status: ItemStatus::Translated,
                        source_hash: "hash".to_string(),
                        meta: serde_json::json!({}),
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn analysis() -> BookAnalysis {
    BookAnalysis {
        genre: "fiction".to_string(),
        tone: "neutral".to_string(),
        style_guide: vec!["Natural Chinese".to_string()],
        narration: "third person".to_string(),
        pacing: "steady".to_string(),
        register: "neutral".to_string(),
        dialogue_style: "plain".to_string(),
        rhetoric: "plain".to_string(),
        characters: Vec::new(),
        terms: Vec::new(),
        book_synopsis: Some("synopsis".to_string()),
    }
}

struct Fixture {
    dir: PathBuf,
    state_dir: PathBuf,
    project: ProjectState,
    chapters: Vec<Chapter>,
}

fn fixture(chapters: usize, segments: usize) -> Fixture {
    let dir = temp_dir();
    let source = dir.join("book.txt");
    std::fs::write(&source, "book").expect("source should be written");
    let state_dir = dir.join("projects");
    let initialized = state::initialize(&state_dir, &source, &document(chapters, segments), 1_200)
        .expect("project should initialize");
    let chapters =
        state::load_chapters(&state_dir, &initialized.project).expect("chapters should load");
    Fixture {
        dir,
        state_dir,
        project: initialized.project,
        chapters,
    }
}

struct ControlledClient {
    delay_ms: u64,
    slow_ids: HashSet<String>,
    fail_ids: HashSet<String>,
    active: AtomicUsize,
    max_active: AtomicUsize,
    processed: Mutex<Vec<String>>,
}

impl ControlledClient {
    fn new(delay_ms: u64) -> Self {
        Self {
            delay_ms,
            slow_ids: HashSet::new(),
            fail_ids: HashSet::new(),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            processed: Mutex::new(Vec::new()),
        }
    }

    fn slow(mut self, id: &str) -> Self {
        self.slow_ids.insert(id.to_string());
        self
    }

    fn fail(mut self, id: &str) -> Self {
        self.fail_ids.insert(id.to_string());
        self
    }
}

#[async_trait]
impl TranslationClient for ControlledClient {
    async fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionOutput, String> {
        assert!(
            system_prompt.contains("TASK:POLISH"),
            "polish round must only issue polish requests"
        );
        let value: serde_json::Value =
            serde_json::from_str(user_prompt).expect("prompt should be JSON");
        let segments = value["segments"]
            .as_array()
            .expect("prompt should contain segments")
            .clone();
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        let slow = segments.iter().any(|segment| {
            self.slow_ids
                .contains(segment["id"].as_str().unwrap_or_default())
        });
        tokio::time::sleep(Duration::from_millis(if slow {
            self.delay_ms * 4
        } else {
            self.delay_ms
        }))
        .await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        let mut translations = Vec::new();
        for segment in segments.iter() {
            let id = segment["id"].as_str().unwrap_or_default().to_string();
            if self.fail_ids.contains(&id) {
                return Err("planned polish failure".to_string());
            }
            self.processed
                .lock()
                .expect("processed mutex")
                .push(id.clone());
            translations.push(format!(
                "已润色：{}",
                segment["translation"].as_str().unwrap_or_default()
            ));
        }
        Ok(CompletionOutput {
            text: serde_json::json!({ "translations": translations }).to_string(),
            usage: Usage::default(),
        })
    }
}

fn config(concurrency: usize) -> AppConfig {
    let mut config = AppConfig::default();
    config.general.polish_concurrency = concurrency;
    config.llm.max_retries = 0;
    config
}

fn draft_for(id: &str) -> String {
    format!("draft {}", id.trim_start_matches('c').replace("-s", "-"))
}

#[test]
fn plan_batches_keeps_chapters_separate_and_skips_finished_segments() {
    let mut document = document(2, 3);
    document.chapters[0].segments[1].polish_status = Some(PolishStatus::Succeeded);
    document.chapters[1].segments[0].target_before_polish = None;
    let batches = plan_batches("round", &document.chapters, 1);

    assert_eq!(batches.len(), 4);
    assert_eq!(batches[0].chapter_id, "chapter-0");
    assert_eq!(batches[0].segment_ids, vec!["c0-s0"]);
    assert_eq!(batches[1].segment_ids, vec!["c0-s2"]);
    assert_eq!(batches[2].chapter_id, "chapter-1");
    assert_eq!(batches[2].segment_ids, vec!["c1-s1"]);
    assert_eq!(batches[3].segment_ids, vec!["c1-s2"]);
    assert!(batches.iter().all(|batch| batch.id.starts_with("round-b")));
}

#[tokio::test]
async fn parallel_polish_respects_the_concurrency_limit() {
    let mut fixture = fixture(4, 2);
    let client = Arc::new(ControlledClient::new(20));
    let summary = run_round(
        client.clone(),
        &fixture.state_dir,
        &fixture.project,
        &mut fixture.chapters,
        &analysis(),
        &[],
        &config(2),
        false,
    )
    .await
    .expect("polish should complete");

    assert!(summary.finished);
    assert_eq!(summary.failed, 0);
    assert_eq!(summary.succeeded, 4);
    assert!(
        client.max_active.load(Ordering::SeqCst) <= 2,
        "in-flight requests must respect polish_concurrency"
    );
    for chapter in &fixture.chapters {
        for segment in &chapter.segments {
            assert!(segment
                .target
                .as_deref()
                .is_some_and(|value| value.starts_with("已润色：")));
            assert_eq!(
                segment.target_before_polish.as_deref(),
                Some(draft_for(&segment.id).as_str())
            );
            assert_eq!(segment.polish_status, Some(PolishStatus::Succeeded));
        }
    }
    std::fs::remove_dir_all(fixture.dir).expect("temp directory should be removed");
}

#[tokio::test]
async fn out_of_order_results_are_applied_in_book_order() {
    let mut fixture = fixture(2, 1);
    let client = Arc::new(ControlledClient::new(10).slow("c0-s0"));
    run_round(
        client.clone(),
        &fixture.state_dir,
        &fixture.project,
        &mut fixture.chapters,
        &analysis(),
        &[],
        &config(2),
        false,
    )
    .await
    .expect("polish should complete");

    let processed = client.processed.lock().expect("processed mutex").clone();
    assert_eq!(processed, vec!["c1-s0", "c0-s0"]);
    for chapter in &fixture.chapters {
        for segment in &chapter.segments {
            assert!(segment.target.as_deref().is_some_and(
                |value| value.starts_with(&format!("已润色：{}", draft_for(&segment.id)))
            ));
        }
    }
    std::fs::remove_dir_all(fixture.dir).expect("temp directory should be removed");
}

#[tokio::test]
async fn failed_batch_keeps_the_draft_and_can_be_retried() {
    let mut fixture = fixture(2, 1);
    let failing = Arc::new(ControlledClient::new(0).fail("c1-s0"));
    let summary = run_round(
        failing,
        &fixture.state_dir,
        &fixture.project,
        &mut fixture.chapters,
        &analysis(),
        &[],
        &config(2),
        false,
    )
    .await
    .expect("failed batches should not abort the round");

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.succeeded, 1);
    let failed = &fixture.chapters[1].segments[0];
    assert_eq!(failed.target.as_deref(), Some("draft 1-0"));
    assert_eq!(failed.polish_status, Some(PolishStatus::Failed));
    assert_eq!(
        fixture.chapters[0].segments[0].polish_status,
        Some(PolishStatus::Succeeded)
    );

    let retry = Arc::new(ControlledClient::new(0));
    let summary = run_round(
        retry,
        &fixture.state_dir,
        &fixture.project,
        &mut fixture.chapters,
        &analysis(),
        &[],
        &config(2),
        true,
    )
    .await
    .expect("retry should complete");
    assert_eq!(summary.failed, 0);
    assert_eq!(summary.succeeded, 2);
    assert_eq!(
        fixture.chapters[1].segments[0].polish_status,
        Some(PolishStatus::Succeeded)
    );
    std::fs::remove_dir_all(fixture.dir).expect("temp directory should be removed");
}

#[tokio::test]
async fn interrupted_round_resumes_with_the_same_snapshot() {
    let mut fixture = fixture(1, 2);
    let config = config(1);
    state::write_json_atomic(
        &round_path(&fixture.state_dir, &fixture.project.id),
        &PolishRound {
            id: "round-interrupted".to_string(),
            created_at: now(),
            updated_at: now(),
            input_digest: drafts_digest(&fixture.chapters),
            terms_digest: terms_digest(&[]),
            terms: Vec::new(),
            style_guide: vec!["Natural Chinese".to_string()],
            book_synopsis: Some("synopsis".to_string()),
            chapter_digests: BTreeMap::new(),
            batches: vec![
                PolishBatch {
                    id: "round-interrupted-b000".to_string(),
                    chapter_id: "chapter-0".to_string(),
                    segment_ids: vec!["c0-s0".to_string()],
                    status: PolishBatchStatus::Running,
                    attempts: 1,
                    error: None,
                    elapsed_ms: None,
                },
                PolishBatch {
                    id: "round-interrupted-b001".to_string(),
                    chapter_id: "chapter-0".to_string(),
                    segment_ids: vec!["c0-s1".to_string()],
                    status: PolishBatchStatus::Pending,
                    attempts: 0,
                    error: None,
                    elapsed_ms: None,
                },
            ],
            finished: false,
            last_error: None,
        },
    )
    .expect("round should be written");

    let summary = run_round(
        Arc::new(ControlledClient::new(0)),
        &fixture.state_dir,
        &fixture.project,
        &mut fixture.chapters,
        &analysis(),
        &[],
        &config,
        false,
    )
    .await
    .expect("interrupted round should resume");

    assert_eq!(summary.round_id, "round-interrupted");
    assert_eq!(summary.succeeded, 2);
    assert!(fixture.chapters[0]
        .segments
        .iter()
        .all(|segment| segment.polish_status == Some(PolishStatus::Succeeded)));
    std::fs::remove_dir_all(fixture.dir).expect("temp directory should be removed");
}

#[tokio::test]
async fn cancelled_round_keeps_saved_results_and_resumes() {
    let fixture = fixture(3, 1);
    let client = Arc::new(ControlledClient::new(40));
    let handle = tokio::spawn({
        let state_dir = fixture.state_dir.clone();
        let project = fixture.project.clone();
        let mut chapters = fixture.chapters.clone();
        async move {
            run_round(
                client,
                &state_dir,
                &project,
                &mut chapters,
                &analysis(),
                &[],
                &config(1),
                false,
            )
            .await
        }
    });
    tokio::time::sleep(Duration::from_millis(15)).await;
    handle.abort();
    let _ = handle.await;

    let round: PolishRound = state::read_json(&round_path(&fixture.state_dir, &fixture.project.id))
        .expect("interrupted round should persist");
    assert!(!round.finished);
    assert!(round.batches.iter().any(|batch| {
        matches!(
            batch.status,
            PolishBatchStatus::Running | PolishBatchStatus::Pending
        )
    }));

    let mut chapters =
        state::load_chapters(&fixture.state_dir, &fixture.project).expect("chapters should load");
    let summary = run_round(
        Arc::new(ControlledClient::new(0)),
        &fixture.state_dir,
        &fixture.project,
        &mut chapters,
        &analysis(),
        &[],
        &config(2),
        false,
    )
    .await
    .expect("interrupted round should resume");
    assert!(summary.finished);
    assert_eq!(summary.failed, 0);
    assert_eq!(summary.succeeded, 3);
    assert!(chapters
        .iter()
        .flat_map(|chapter| &chapter.segments)
        .all(
            |segment| segment.polish_status == Some(PolishStatus::Succeeded)
                && !segment
                    .target
                    .as_deref()
                    .unwrap_or_default()
                    .starts_with("draft")
        ));
    std::fs::remove_dir_all(fixture.dir).expect("temp directory should be removed");
}

#[tokio::test]
async fn changed_draft_invalidates_the_old_snapshot() {
    let mut fixture = fixture(2, 1);
    run_round(
        Arc::new(ControlledClient::new(0)),
        &fixture.state_dir,
        &fixture.project,
        &mut fixture.chapters,
        &analysis(),
        &[],
        &config(2),
        false,
    )
    .await
    .expect("first round should complete");
    let old_round = read_summary(&fixture.state_dir, &fixture.project.id, &fixture.chapters)
        .expect("summary should load")
        .expect("round should exist")
        .round_id;

    fixture.chapters[1].segments[0].target_before_polish = Some("新初稿".to_string());
    fixture.chapters[1].segments[0].target = Some("新初稿".to_string());
    fixture.chapters[1].segments[0].polish_status = Some(PolishStatus::Pending);
    let summary = run_round(
        Arc::new(ControlledClient::new(0)),
        &fixture.state_dir,
        &fixture.project,
        &mut fixture.chapters,
        &analysis(),
        &[],
        &config(2),
        false,
    )
    .await
    .expect("new round should cover the changed draft");

    assert_ne!(summary.round_id, old_round);
    assert_eq!(summary.total, 1);
    assert!(fixture.chapters[1].segments[0]
        .target
        .as_deref()
        .is_some_and(|value| value.starts_with("已润色：新初稿")));
    std::fs::remove_dir_all(fixture.dir).expect("temp directory should be removed");
}

#[tokio::test]
async fn polish_requires_a_complete_translation() {
    let mut fixture = fixture(1, 1);
    fixture.chapters[0].target_title = None;
    let error = run_round(
        Arc::new(ControlledClient::new(0)),
        &fixture.state_dir,
        &fixture.project,
        &mut fixture.chapters,
        &analysis(),
        &[],
        &config(2),
        false,
    )
    .await
    .expect_err("incomplete translation must be rejected");
    assert!(error.contains("全书翻译尚未完成"));
    std::fs::remove_dir_all(fixture.dir).expect("temp directory should be removed");
}

#[tokio::test]
async fn polish_skips_manual_text_and_records_versions_for_other_segments() {
    let mut fixture = fixture(1, 2);
    crate::revisions::set_target(
        &mut fixture.chapters[0].segments[0],
        "人工定稿".into(),
        crate::revisions::RevisionKind::Manual,
        None,
    )
    .unwrap();
    let client = Arc::new(ControlledClient::new(0));
    let summary = run_round(
        client.clone(),
        &fixture.state_dir,
        &fixture.project,
        &mut fixture.chapters,
        &analysis(),
        &[],
        &config(1),
        false,
    )
    .await
    .unwrap();
    assert_eq!(summary.succeeded, 1);
    assert_eq!(*client.processed.lock().unwrap(), vec!["c0-s1"]);
    let chapters = state::load_chapters(&fixture.state_dir, &fixture.project).unwrap();
    assert_eq!(chapters[0].segments[0].target.as_deref(), Some("人工定稿"));
    assert_eq!(
        crate::revisions::history(&chapters[0].segments[0])
            .unwrap()
            .last()
            .unwrap()
            .kind,
        crate::revisions::RevisionKind::Manual
    );
    assert_eq!(
        crate::revisions::history(&chapters[0].segments[1])
            .unwrap()
            .last()
            .unwrap()
            .kind,
        crate::revisions::RevisionKind::Polish
    );
    std::fs::remove_dir_all(fixture.dir).unwrap();
}
