//! 命令行与工作流的集成测试：参数解析、导入/翻译/润色/重译与导出。

use super::args::{Command, ExportArgs, ExportFormatArg};
use super::export::run as export_file;
use super::Cli;
use crate::analysis::BookAnalysis;
use crate::config::AppConfig;
use crate::llm::{CompletionOutput, MockClient, TranslationClient};
use crate::model::{
    Document, DocumentMetadata, ItemStatus, PolishStatus, ProjectStatus, Segment, SegmentKind,
};
use crate::polish;
use crate::terms::TermStore;
use crate::usage::UsageFile;
use crate::workflow::{
    import_project, initialize_project, mark_retranslation_error, polish_project, retranslate_item,
    retranslate_project, run_transit, transit,
};
use crate::{parser, state};
use async_trait::async_trait;
use clap::Parser;
use rig_core::completion::Usage;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "transitpls-cli-{}-{nonce}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).expect("temp directory should be created");
    path
}

#[test]
fn accepts_explicit_config_and_project_id() {
    let cli = Cli::try_parse_from([
        "transitpls-cli",
        "--config",
        "custom.toml",
        "status",
        "--project",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ])
    .expect("CLI arguments should parse");
    assert_eq!(cli.config, Some(PathBuf::from("custom.toml")));
}

#[test]
fn imports_book_without_running_model_analysis() {
    let dir = temp_dir();
    let source = dir.join("book.txt");
    let config_path = dir.join("transitpls.toml");
    let state_dir = dir.join("projects");
    fs::write(&source, "Alice arrived.").unwrap();
    fs::write(
        &config_path,
        format!("[paths]\nstate_dir = {:?}\n", state_dir),
    )
    .unwrap();

    let (project, created) = import_project(Some(config_path.clone()), source.clone()).unwrap();
    assert!(created);
    assert!(!state::project_dir(&state_dir, &project.id)
        .join("analysis.json")
        .exists());

    let (_, created_again) = import_project(Some(config_path), source).unwrap();
    assert!(!created_again);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn initialization_analysis_failure_is_written_to_project_log() {
    let dir = temp_dir();
    let source = dir.join("book.txt");
    let state_dir = dir.join("projects");
    fs::write(&source, "Alice arrived.").unwrap();
    let document = parser::parse_document(&source, Some("en"), 1200).unwrap();
    let initialized = state::initialize(&state_dir, &source, &document, 1200).unwrap();
    let project_dir = state::project_dir(&state_dir, &initialized.project.id);
    fs::write(project_dir.join("analysis.json"), "invalid JSON").unwrap();
    let config_path = dir.join("config.toml");
    fs::write(&config_path, "[paths]\nstate_dir = 'projects'\n").unwrap();
    let error = initialize_project(Some(config_path), source, None, None, true, false)
        .await
        .unwrap_err();
    let log = fs::read_to_string(project_dir.join("logs.txt")).unwrap();
    let entry: serde_json::Value =
        serde_json::from_str(log.lines().last().unwrap().splitn(3, '\t').nth(2).unwrap()).unwrap();
    assert_eq!(entry["error"], error);
    assert!(log.contains("\tfailed\t"));
    assert_eq!(
        state::load_project(&state_dir, &initialized.project.id)
            .unwrap()
            .status,
        crate::model::ProjectStatus::Failed
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn accepts_zero_based_chapter_filter() {
    let cli = Cli::try_parse_from([
        "transitpls-cli",
        "transit",
        "book.txt",
        "--chapter",
        "0",
        "--mock",
    ])
    .expect("CLI arguments should parse");
    let Command::Transit(args) = cli.command else {
        panic!("transit command should parse");
    };
    assert_eq!(args.chapter, Some(0));
    assert!(args.mock);
}

#[test]
fn accepts_polish_command_flags() {
    let cli = Cli::try_parse_from([
        "transitpls-cli",
        "polish",
        "book.txt",
        "--retry-failed",
        "--mock",
    ])
    .expect("polish arguments should parse");
    let Command::Polish(args) = cli.command else {
        panic!("polish command should parse");
    };
    assert!(args.retry_failed);
    assert!(args.mock);
    assert_eq!(args.input, PathBuf::from("book.txt"));
}

#[test]
fn accepts_export_format_and_output_path() {
    let cli = Cli::try_parse_from([
        "transitpls-cli",
        "export",
        "--format",
        "epub",
        "--out",
        "dist/book.epub",
        "book.txt",
    ])
    .expect("export arguments should parse");
    let Command::Export(args) = cli.command else {
        panic!("export command should parse");
    };
    assert_eq!(args.format, ExportFormatArg::Epub);
    assert_eq!(args.out, Some(PathBuf::from("dist/book.epub")));
    assert_eq!(args.input, PathBuf::from("book.txt"));
    assert!(!args.bilingual);
    assert!(args.order.is_none());
    let cli = Cli::try_parse_from([
        "transitpls-cli",
        "export",
        "book.txt",
        "--format",
        "txt",
        "--bilingual",
        "--order",
        "source-first",
    ])
    .unwrap();
    let Command::Export(args) = cli.command else {
        panic!("export expected")
    };
    assert!(args.bilingual);
    assert_eq!(args.order, Some(crate::export::ExportOrder::SourceFirst));
    assert!(Cli::try_parse_from([
        "transitpls-cli",
        "export",
        "book.txt",
        "--format",
        "txt",
        "--bilingual",
        "--order",
        "invalid"
    ])
    .is_err());
    let error = export_file(
        ExportArgs {
            format: ExportFormatArg::Txt,
            out: None,
            input: PathBuf::from("missing.txt"),
            bilingual: false,
            order: args.order,
        },
        std::path::Path::new("missing"),
    )
    .unwrap_err();
    assert!(error.contains("requires bilingual"));
}

#[test]
fn txt_export_overwrites_output_without_changing_project_log() {
    let dir = temp_dir();
    let source = dir.join("book.txt");
    let state_dir = dir.join("projects");
    fs::write(&source, "Chapter 1\n\nHello, world!").expect("source should be written");
    let document =
        parser::parse_document(&source, Some("en"), 1_200).expect("document should parse");
    let initialized = state::initialize(&state_dir, &source, &document, 1_200)
        .expect("project should initialize");
    let mut project = initialized.project;
    let mut chapters = state::load_chapters(&state_dir, &project).expect("chapters should load");
    chapters[0].target_title = Some("第一章".to_string());
    chapters[0].segments[0].target = Some("你好, 世界!".to_string());
    chapters[0].segments[0].status = ItemStatus::Translated;
    let lock =
        state::acquire_project_lock(&state_dir, &project).expect("project lock should be created");
    state::save_progress(&state_dir, &mut project, &mut chapters)
        .expect("translation state should save");
    drop(lock);
    let project_dir = state::project_dir(&state_dir, &project.id);
    let log_path = project_dir.join("logs.txt");
    let log_before = fs::read(&log_path).expect("log should be readable");
    let output = dir.join("output/book.zh.txt");
    fs::create_dir_all(output.parent().expect("output should have parent"))
        .expect("output directory should be created");
    fs::write(&output, "old output").expect("old output should be written");

    let result = export_file(
        ExportArgs {
            format: ExportFormatArg::Txt,
            out: None,
            bilingual: false,
            order: None,
            input: source.clone(),
        },
        &state_dir,
    )
    .expect("TXT export should succeed");

    assert_eq!(result, 0);
    assert_eq!(
        fs::read_to_string(output).expect("output should be readable"),
        "第一章\n\n你好， 世界！\n"
    );
    assert_eq!(
        fs::read(&log_path).expect("log should remain readable"),
        log_before
    );
    let saved = serde_json::to_value(state::load_chapters(&state_dir, &project).unwrap()).unwrap();
    for order in [None, Some(crate::export::ExportOrder::SourceFirst)] {
        let custom = order.map(|_| dir.join("custom.txt"));
        export_file(
            ExportArgs {
                format: ExportFormatArg::Txt,
                out: custom.clone(),
                input: source.clone(),
                bilingual: true,
                order,
            },
            &state_dir,
        )
        .unwrap();
        let contents =
            fs::read_to_string(custom.unwrap_or_else(|| dir.join("output/book.zh-bi.txt")))
                .unwrap();
        assert_eq!(
            contents,
            if order.is_some() {
                "第一章\n\nHello, world!\n你好， 世界！\n"
            } else {
                "第一章\n\n你好， 世界！\nHello, world!\n"
            }
        );
    }
    assert_eq!(
        serde_json::to_value(state::load_chapters(&state_dir, &project).unwrap()).unwrap(),
        saved
    );
    assert_eq!(fs::read(&log_path).unwrap(), log_before);
    fs::remove_dir_all(dir).expect("temp directory should be removed");
}

#[tokio::test]
async fn mock_transit_extracts_terms_after_saving_translation() {
    let dir = temp_dir();
    let source = dir.join("book.txt");
    let state_dir = dir.join("projects");
    fs::write(&source, "Chapter 1\n\nAlice entered the city.").expect("source should be written");
    let document =
        parser::parse_document(&source, Some("en"), 1_200).expect("document should parse");
    let initialized = state::initialize(&state_dir, &source, &document, 1_200)
        .expect("project should initialize");
    let mut project = initialized.project.clone();
    let mut chapters = state::load_chapters(&state_dir, &project).expect("chapters should load");
    crate::analysis::prepare(
        &MockClient,
        &state_dir,
        &mut project,
        &mut chapters,
        true,
        false,
        0,
    )
    .await
    .expect("mock analysis should complete");

    transit(
        source.clone(),
        None,
        true,
        &state_dir,
        &AppConfig::default(),
    )
    .await
    .expect("mock transit should complete");

    let chapters =
        state::load_chapters(&state_dir, &initialized.project).expect("chapters should load");
    assert!(chapters[0]
        .segments
        .iter()
        .all(|segment| segment.target.is_some()));
    assert!(chapters[0].target_title.is_some());
    assert_eq!(
        chapters[0].meta["terms_extracted"],
        serde_json::Value::Bool(true)
    );
    assert!(chapters[0].meta.get("pending_term_extractions").is_none());
    let store =
        TermStore::open(state::project_dir(&state_dir, &initialized.project.id).join("terms.db"))
            .expect("term store should open");
    assert!(store
        .list()
        .expect("terms should list")
        .iter()
        .any(|term| term.source == "Alice"));
    assert!(store
        .pending_extractions()
        .expect("pending extractions should list")
        .is_empty());
    let project_dir = state::project_dir(&state_dir, &initialized.project.id);
    assert!(project_dir.join("context.json").is_file());
    let usage: UsageFile =
        state::read_json(&project_dir.join("usage.json")).expect("usage should load");
    assert!(usage.calls.iter().any(|entry| entry.stage == "translation"));
    assert!(usage
        .calls
        .iter()
        .any(|entry| entry.stage == "title_translation"));

    let first_target = chapters[0].segments[0].target.clone();
    let call_count = usage.calls.len();
    transit(
        source.clone(),
        None,
        true,
        &state_dir,
        &AppConfig::default(),
    )
    .await
    .expect("completed transit should be resumable");
    let chapters =
        state::load_chapters(&state_dir, &initialized.project).expect("chapters should reload");
    let usage: UsageFile =
        state::read_json(&project_dir.join("usage.json")).expect("usage should reload");
    assert_eq!(chapters[0].segments[0].target, first_target);
    assert_eq!(usage.calls.len(), call_count);
    drop(store);
    fs::remove_dir_all(dir).expect("temp directory should be removed");
}

#[tokio::test]
async fn mock_polish_preserves_draft_and_saves_polished_target() {
    let dir = temp_dir();
    let source = dir.join("book.txt");
    let state_dir = dir.join("projects");
    fs::write(&source, "Chapter 1\n\nAlice arrived.").expect("source should be written");
    let document =
        parser::parse_document(&source, Some("en"), 1_200).expect("document should parse");
    let initialized = state::initialize(&state_dir, &source, &document, 1_200)
        .expect("project should initialize");
    let mut project = initialized.project.clone();
    let mut chapters = state::load_chapters(&state_dir, &project).expect("chapters should load");
    crate::analysis::prepare(
        &MockClient,
        &state_dir,
        &mut project,
        &mut chapters,
        true,
        false,
        0,
    )
    .await
    .expect("mock analysis should complete");
    let mut config = AppConfig::default();
    config.pipeline.polish = true;

    transit(source, None, true, &state_dir, &config)
        .await
        .expect("polished transit should complete");

    let chapters =
        state::load_chapters(&state_dir, &initialized.project).expect("chapters should reload");
    let segment = &chapters[0].segments[0];
    assert!(segment
        .target_before_polish
        .as_deref()
        .is_some_and(|value| value.starts_with("[mock zh-CN]")));
    assert!(segment
        .target
        .as_deref()
        .is_some_and(|value| value.starts_with("[mock polished zh-CN]")));
    assert_eq!(segment.polish_status, Some(PolishStatus::Succeeded));
    let summary = polish::read_summary(&state_dir, &initialized.project.id, &chapters)
        .expect("polish summary should load")
        .expect("auto polish should create a round");
    assert!(summary.finished);
    assert_eq!(summary.failed, 0);
    fs::remove_dir_all(dir).expect("temp directory should be removed");
}

#[tokio::test]
async fn manual_polish_polishes_saved_drafts_without_auto_enabled() {
    let dir = temp_dir();
    let source = dir.join("book.txt");
    let state_dir = dir.join("projects");
    let config_path = dir.join("config.toml");
    fs::write(&source, "Chapter 1\n\nAlice arrived.").expect("source should be written");
    fs::write(
        &config_path,
        format!("[paths]\nstate_dir = {state_dir:?}\n[pipeline]\npolish = false\n"),
    )
    .expect("config should be written");
    let document =
        parser::parse_document(&source, Some("en"), 1_200).expect("document should parse");
    let initialized = state::initialize(&state_dir, &source, &document, 1_200)
        .expect("project should initialize");
    let mut project = initialized.project.clone();
    let mut chapters = state::load_chapters(&state_dir, &project).expect("chapters should load");
    crate::analysis::prepare(
        &MockClient,
        &state_dir,
        &mut project,
        &mut chapters,
        true,
        false,
        0,
    )
    .await
    .expect("mock analysis should complete");

    transit(
        source.clone(),
        None,
        true,
        &state_dir,
        &AppConfig::default(),
    )
    .await
    .expect("transit should complete");

    let chapters =
        state::load_chapters(&state_dir, &initialized.project).expect("chapters should reload");
    let draft = chapters[0].segments[0]
        .target_before_polish
        .clone()
        .expect("draft should be saved");
    assert_eq!(
        chapters[0].segments[0].target.as_deref(),
        Some(draft.as_str())
    );
    assert!(
        polish::read_summary(&state_dir, &initialized.project.id, &chapters)
            .expect("summary should load")
            .is_none()
    );

    polish_project(Some(config_path), source, false, true)
        .await
        .expect("manual polish should complete");

    let chapters =
        state::load_chapters(&state_dir, &initialized.project).expect("chapters should reload");
    assert_eq!(
        chapters[0].segments[0].target_before_polish.as_deref(),
        Some(draft.as_str())
    );
    assert!(chapters[0].segments[0]
        .target
        .as_deref()
        .is_some_and(|value| value.starts_with("[mock polished zh-CN]")));
    assert_eq!(
        chapters[0].segments[0].polish_status,
        Some(PolishStatus::Succeeded)
    );
    fs::remove_dir_all(dir).expect("temp directory should be removed");
}

#[tokio::test]
async fn single_chapter_translation_does_not_auto_polish() {
    let dir = temp_dir();
    let source = dir.join("book.txt");
    fs::write(&source, "book").expect("source should be written");
    let state_dir = dir.join("projects");
    let document = Document {
        metadata: DocumentMetadata {
            title: "Book".to_string(),
            source_language: "en".to_string(),
            target_language: "zh-CN".to_string(),
            source_format: "txt".to_string(),
        },
        chapters: vec![
            crate::model::Chapter {
                id: "chapter-1".to_string(),
                title: "First".to_string(),
                target_title: None,
                status: ItemStatus::Pending,
                meta: serde_json::json!({}),
                segments: vec![test_segment("one", "First paragraph.")],
            },
            crate::model::Chapter {
                id: "chapter-2".to_string(),
                title: "Second".to_string(),
                target_title: None,
                status: ItemStatus::Pending,
                meta: serde_json::json!({}),
                segments: vec![test_segment("two", "Second paragraph.")],
            },
        ],
    };
    let initialized = state::initialize(&state_dir, &source, &document, 1_200)
        .expect("project should initialize");
    let mut project = initialized.project.clone();
    let mut chapters = state::load_chapters(&state_dir, &project).expect("chapters should load");
    crate::analysis::prepare(
        &MockClient,
        &state_dir,
        &mut project,
        &mut chapters,
        false,
        false,
        0,
    )
    .await
    .expect("mock analysis should complete");
    let mut config = AppConfig::default();
    config.pipeline.polish = true;
    config.analysis.full_book = false;

    transit(source, Some(0), true, &state_dir, &config)
        .await
        .expect("single chapter transit should complete");

    let chapters =
        state::load_chapters(&state_dir, &initialized.project).expect("chapters should reload");
    assert!(chapters[0].segments[0].target.is_some());
    assert!(chapters[1].segments[0].target.is_none());
    assert!(
        polish::read_summary(&state_dir, &initialized.project.id, &chapters)
            .expect("summary should load")
            .is_none()
    );
    fs::remove_dir_all(dir).expect("temp directory should be removed");
}

struct BatchRejectingClient {
    translation_requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

#[async_trait]
impl TranslationClient for BatchRejectingClient {
    async fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionOutput, String> {
        if system_prompt.contains("TASK:TERM_EXTRACTION") {
            return Ok(output(r#"{"terms":[]}"#.to_string()));
        }
        let value: serde_json::Value =
            serde_json::from_str(user_prompt).expect("prompt should be JSON");
        let segments = value["segments"]
            .as_array()
            .expect("prompt should contain segments");
        if system_prompt.contains("TASK:TRANSLATION") {
            self.translation_requests
                .lock()
                .expect("requests mutex")
                .push(value.clone());
            if segments.len() > 1 {
                return Ok(output("not json".to_string()));
            }
        }
        Ok(output(
            serde_json::json!({
                "translations": segments.iter()
                    .map(|segment| format!("translated {}", segment["source"].as_str().unwrap_or_default()))
                    .collect::<Vec<_>>()
            })
            .to_string(),
        ))
    }
}

fn output(text: String) -> CompletionOutput {
    CompletionOutput {
        text,
        usage: Usage::default(),
    }
}

#[tokio::test]
async fn failed_batch_falls_back_to_individual_segments() {
    let dir = temp_dir();
    let source = dir.join("book.txt");
    fs::write(&source, "book").expect("source should be written");
    let state_dir = dir.join("projects");
    let document = Document {
        metadata: DocumentMetadata {
            title: "Book".to_string(),
            source_language: "en".to_string(),
            target_language: "zh-CN".to_string(),
            source_format: "txt".to_string(),
        },
        chapters: vec![crate::model::Chapter {
            id: "chapter-1".to_string(),
            title: "Opening".to_string(),
            target_title: None,
            status: ItemStatus::Pending,
            meta: serde_json::json!({ "source_digest": "digest" }),
            segments: vec![
                test_segment("one", "First paragraph."),
                test_segment("two", "Second paragraph."),
            ],
        }],
    };
    let initialized = state::initialize(&state_dir, &source, &document, 1_200)
        .expect("project should initialize");
    let mut project = initialized.project;
    let mut chapters = state::load_chapters(&state_dir, &project).expect("chapters should load");
    let store = TermStore::open(state::project_dir(&state_dir, &project.id).join("terms.db"))
        .expect("term store should open");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let client: Arc<dyn TranslationClient> = Arc::new(BatchRejectingClient {
        translation_requests: Arc::clone(&requests),
    });
    let analysis = test_analysis();
    let mut config = AppConfig::default();
    config.llm.max_retries = 0;
    config.pipeline.polish = true;
    config.general.translation_concurrency = 2;

    run_transit(
        client,
        &store,
        &state_dir,
        &mut project,
        &mut chapters,
        &analysis,
        &config,
        None,
    )
    .await
    .expect("individual fallback should complete");

    let requests = requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|request| request["segments"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        vec![2, 1, 1]
    );
    assert_eq!(requests[1]["surrounding_source"]["before"], "");
    assert_eq!(
        requests[1]["surrounding_source"]["after"],
        "Second paragraph."
    );
    assert_eq!(
        requests[2]["surrounding_source"]["before"],
        "First paragraph."
    );
    assert_eq!(requests[2]["surrounding_source"]["after"], "");
    assert!(chapters[0]
        .segments
        .iter()
        .all(|segment| segment.status == ItemStatus::Translated));
    assert_eq!(project.status, ProjectStatus::Translated);
    // 兜底路径不应内联执行润色，即使开启了自动润色。
    assert!(chapters[0].segments.iter().all(|segment| {
        segment.target == segment.target_before_polish
            && segment.polish_status == Some(PolishStatus::Pending)
    }));
    assert!(polish::read_summary(&state_dir, &project.id, &chapters)
        .expect("summary should load")
        .is_none());
    drop(store);
    fs::remove_dir_all(dir).expect("temp directory should be removed");
}

struct TransitFixture {
    dir: PathBuf,
    state_dir: PathBuf,
    project: crate::model::ProjectState,
    chapters: Vec<crate::model::Chapter>,
    store: TermStore,
}

fn transit_fixture(document: Document) -> TransitFixture {
    let dir = temp_dir();
    let source = dir.join("book.txt");
    fs::write(&source, "book").expect("source should be written");
    let state_dir = dir.join("projects");
    let initialized = state::initialize(&state_dir, &source, &document, 1_200)
        .expect("project should initialize");
    let chapters =
        state::load_chapters(&state_dir, &initialized.project).expect("chapters should load");
    let store =
        TermStore::open(state::project_dir(&state_dir, &initialized.project.id).join("terms.db"))
            .expect("term store should open");
    TransitFixture {
        dir,
        state_dir,
        project: initialized.project,
        chapters,
        store,
    }
}

fn pending_document(chapter_count: usize, segments_per_chapter: usize) -> Document {
    Document {
        metadata: DocumentMetadata {
            title: "Book".to_string(),
            source_language: "en".to_string(),
            target_language: "zh-CN".to_string(),
            source_format: "txt".to_string(),
        },
        chapters: (0..chapter_count)
            .map(|chapter| crate::model::Chapter {
                id: format!("chapter-{chapter}"),
                title: format!("Chapter {chapter}"),
                target_title: None,
                status: ItemStatus::Pending,
                meta: serde_json::json!({ "source_digest": format!("digest {chapter}") }),
                segments: (0..segments_per_chapter)
                    .map(|segment| {
                        test_segment(
                            &format!("c{chapter}-s{segment}"),
                            &format!("Paragraph {chapter}-{segment}."),
                        )
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// 记录同时在飞的翻译批次数，并可让指定原文的批次变慢，用于制造乱序完成。
struct ConcurrencyProbeClient {
    delay_ms: u64,
    slow_source: Option<String>,
    active: AtomicUsize,
    max_active: AtomicUsize,
    requests: Mutex<Vec<serde_json::Value>>,
}

impl ConcurrencyProbeClient {
    fn new(delay_ms: u64) -> Self {
        Self {
            delay_ms,
            slow_source: None,
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn slow_on(mut self, source: &str) -> Self {
        self.slow_source = Some(source.to_string());
        self
    }
}

#[async_trait]
impl TranslationClient for ConcurrencyProbeClient {
    async fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionOutput, String> {
        if system_prompt.contains("TASK:TERM_EXTRACTION") {
            return Ok(output(r#"{"terms":[]}"#.to_string()));
        }
        let value: serde_json::Value =
            serde_json::from_str(user_prompt).map_err(|error| error.to_string())?;
        let segments = value["segments"].as_array().cloned().unwrap_or_default();
        let translations = output(
            serde_json::json!({
                "translations": segments
                    .iter()
                    .map(|segment| format!(
                        "translated {}",
                        segment["source"].as_str().unwrap_or_default()
                    ))
                    .collect::<Vec<_>>()
            })
            .to_string(),
        );
        if !system_prompt.contains("TASK:TRANSLATION") {
            return Ok(translations);
        }
        self.requests.lock().unwrap().push(value);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        let slow = self.slow_source.as_ref().is_some_and(|source| {
            segments
                .iter()
                .any(|segment| segment["source"].as_str() == Some(source.as_str()))
        });
        tokio::time::sleep(Duration::from_millis(if slow {
            self.delay_ms * 5
        } else {
            self.delay_ms
        }))
        .await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(translations)
    }
}

fn concurrent_config(concurrency: usize) -> AppConfig {
    let mut config = AppConfig::default();
    config.llm.max_retries = 0;
    config.segment.max_chars_per_batch = 1;
    config.general.translation_concurrency = concurrency;
    config
}

#[tokio::test]
async fn concurrent_translation_limits_in_flight_batches() {
    let mut fixture = transit_fixture(pending_document(2, 3));
    let probe = Arc::new(ConcurrencyProbeClient::new(10));
    let client: Arc<dyn TranslationClient> = probe.clone();

    run_transit(
        client,
        &fixture.store,
        &fixture.state_dir,
        &mut fixture.project,
        &mut fixture.chapters,
        &test_analysis(),
        &concurrent_config(2),
        None,
    )
    .await
    .expect("concurrent transit should complete");

    assert_eq!(
        probe.max_active.load(Ordering::SeqCst),
        2,
        "in-flight translation batches must respect translation_concurrency"
    );
    let requests = probe.requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    for chapter in &fixture.chapters {
        for (index, segment) in chapter.segments.iter().enumerate() {
            let expected = format!("translated {}", segment.source);
            assert_eq!(segment.target.as_deref(), Some(expected.as_str()));
            assert_eq!(segment.status, ItemStatus::Translated);
            let request = requests
                .iter()
                .find(|request| request["segments"][0]["id"] == segment.id)
                .unwrap();
            assert_eq!(request["segments"].as_array().unwrap().len(), 1);
            assert!(request["recent_targets"].as_array().unwrap().is_empty());
            let before = index
                .checked_sub(1)
                .map(|index| chapter.segments[index].source.as_str())
                .unwrap_or_default();
            let after = chapter
                .segments
                .get(index + 1)
                .map(|segment| segment.source.as_str())
                .unwrap_or_default();
            assert_eq!(request["surrounding_source"]["before"], before);
            assert_eq!(request["surrounding_source"]["after"], after);
        }
    }
    drop(fixture.store);
    fs::remove_dir_all(fixture.dir).expect("temp directory should be removed");
}

#[tokio::test]
async fn out_of_order_batch_completion_writes_back_by_position() {
    let mut fixture = transit_fixture(pending_document(2, 1));
    let probe = Arc::new(ConcurrencyProbeClient::new(10).slow_on("Paragraph 0-0."));
    let client: Arc<dyn TranslationClient> = probe.clone();

    run_transit(
        client,
        &fixture.store,
        &fixture.state_dir,
        &mut fixture.project,
        &mut fixture.chapters,
        &test_analysis(),
        &concurrent_config(2),
        None,
    )
    .await
    .expect("out-of-order completion should still write back correctly");

    // 第二批先完成，写回仍必须落到各自段落的原位置。
    for chapter in &fixture.chapters {
        for segment in &chapter.segments {
            let expected = format!("translated {}", segment.source);
            assert_eq!(segment.target.as_deref(), Some(expected.as_str()));
        }
    }
    assert_eq!(fixture.project.chapters_completed, 2);
    assert_eq!(fixture.project.status, ProjectStatus::Translated);
    drop(fixture.store);
    fs::remove_dir_all(fixture.dir).expect("temp directory should be removed");
}

#[tokio::test]
async fn serial_translation_sends_one_batch_at_a_time() {
    let mut fixture = transit_fixture(pending_document(2, 2));
    let probe = Arc::new(ConcurrencyProbeClient::new(5));
    let client: Arc<dyn TranslationClient> = probe.clone();

    run_transit(
        client,
        &fixture.store,
        &fixture.state_dir,
        &mut fixture.project,
        &mut fixture.chapters,
        &test_analysis(),
        &concurrent_config(1),
        None,
    )
    .await
    .expect("serial transit should complete");

    assert_eq!(probe.max_active.load(Ordering::SeqCst), 1);
    for chapter in &fixture.chapters {
        for segment in &chapter.segments {
            let expected = format!("translated {}", segment.source);
            assert_eq!(segment.target.as_deref(), Some(expected.as_str()));
        }
    }
    drop(fixture.store);
    fs::remove_dir_all(fixture.dir).expect("temp directory should be removed");
}

#[tokio::test]
async fn selected_chapter_leaves_other_chapters_untouched() {
    let dir = temp_dir();
    let source = dir.join("book.txt");
    fs::write(&source, "book").expect("source should be written");
    let state_dir = dir.join("projects");
    let document = Document {
        metadata: DocumentMetadata {
            title: "Book".to_string(),
            source_language: "en".to_string(),
            target_language: "zh-CN".to_string(),
            source_format: "txt".to_string(),
        },
        chapters: vec![
            crate::model::Chapter {
                id: "chapter-1".to_string(),
                title: "First".to_string(),
                target_title: None,
                status: ItemStatus::Pending,
                meta: serde_json::json!({ "source_digest": "first digest" }),
                segments: vec![test_segment("one", "First paragraph.")],
            },
            crate::model::Chapter {
                id: "chapter-2".to_string(),
                title: "Second".to_string(),
                target_title: None,
                status: ItemStatus::Pending,
                meta: serde_json::json!({ "source_digest": "second digest" }),
                segments: vec![test_segment("two", "Second paragraph.")],
            },
        ],
    };
    let initialized = state::initialize(&state_dir, &source, &document, 1_200)
        .expect("project should initialize");
    let mut project = initialized.project;
    let mut chapters = state::load_chapters(&state_dir, &project).expect("chapters should load");
    let store = TermStore::open(state::project_dir(&state_dir, &project.id).join("terms.db"))
        .expect("term store should open");

    run_transit(
        Arc::new(MockClient),
        &store,
        &state_dir,
        &mut project,
        &mut chapters,
        &test_analysis(),
        &AppConfig::default(),
        Some(1),
    )
    .await
    .expect("selected chapter should translate");

    assert!(chapters[0].segments[0].target.is_none());
    assert!(chapters[1].segments[0].target.is_some());
    assert!(chapters
        .iter()
        .all(|chapter| chapter.target_title.is_none()));
    assert_eq!(project.chapters_completed, 1);
    assert_eq!(project.status, ProjectStatus::Translating);
    drop(store);
    fs::remove_dir_all(dir).expect("temp directory should be removed");
}

struct RetranslationFailure;

#[async_trait]
impl TranslationClient for RetranslationFailure {
    async fn complete(
        &self,
        _system_prompt: &str,
        _user_prompt: &str,
    ) -> Result<CompletionOutput, String> {
        Err("planned retranslation failure".to_string())
    }
}

#[tokio::test]
async fn retranslation_keeps_failed_text_and_saves_a_recovery_version_on_success() {
    let dir = temp_dir();
    let source = dir.join("book.txt");
    fs::write(&source, "book").unwrap();
    let state_dir = dir.join("projects");
    let mut segment = test_segment("segment-1", "Alice arrived.");
    segment.target = Some("旧译文".to_string());
    segment.status = ItemStatus::Translated;
    let document = Document {
        metadata: DocumentMetadata {
            title: "Book".to_string(),
            source_language: "en".to_string(),
            target_language: "zh-CN".to_string(),
            source_format: "txt".to_string(),
        },
        chapters: vec![crate::model::Chapter {
            id: "chapter-1".to_string(),
            title: "Chapter 1".to_string(),
            target_title: Some("第一章".to_string()),
            status: ItemStatus::Translated,
            meta: serde_json::json!({}),
            segments: vec![segment],
        }],
    };
    let initialized = state::initialize(&state_dir, &source, &document, 1_200).unwrap();
    let mut project = initialized.project;
    let mut chapters = state::load_chapters(&state_dir, &project).unwrap();
    let store =
        TermStore::open(state::project_dir(&state_dir, &project.id).join("terms.db")).unwrap();
    let mut config = AppConfig::default();
    config.llm.max_retries = 0;

    let error = retranslate_item(
        &RetranslationFailure,
        &store,
        &state_dir,
        &mut project,
        &mut chapters,
        &test_analysis(),
        &config,
        "segment-1",
    )
    .await
    .unwrap_err();
    mark_retranslation_error(&state_dir, &project, &mut chapters, "segment-1", &error).unwrap();
    assert_eq!(chapters[0].segments[0].target.as_deref(), Some("旧译文"));
    assert!(chapters[0].segments[0].meta["retranslation_error"]
        .as_str()
        .unwrap()
        .contains("planned retranslation failure"));

    retranslate_item(
        &MockClient,
        &store,
        &state_dir,
        &mut project,
        &mut chapters,
        &test_analysis(),
        &config,
        "segment-1",
    )
    .await
    .unwrap();
    assert_eq!(chapters[0].segments[0].meta["previous_target"], "旧译文");
    assert!(chapters[0].segments[0]
        .meta
        .get("retranslation_error")
        .is_none());
    assert_ne!(chapters[0].segments[0].target.as_deref(), Some("旧译文"));
    drop(store);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn project_retranslation_reports_each_completed_item() {
    let dir = temp_dir();
    let source = dir.join("book.txt");
    let state_dir = dir.join("projects");
    let config_path = dir.join("config.toml");
    fs::write(&source, "book").unwrap();
    fs::write(
        &config_path,
        "[paths]\nstate_dir = 'projects'\n[pipeline]\npolish = false\n[analysis]\nfull_book = false\n",
    )
    .unwrap();
    let mut segment = test_segment("segment-1", "Alice arrived.");
    segment.target = Some("旧译文".to_string());
    segment.status = ItemStatus::Translated;
    let document = Document {
        metadata: DocumentMetadata {
            title: "Book".to_string(),
            source_language: "en".to_string(),
            target_language: "zh-CN".to_string(),
            source_format: "txt".to_string(),
        },
        chapters: vec![crate::model::Chapter {
            id: "chapter-1".to_string(),
            title: "Chapter 1".to_string(),
            target_title: Some("第一章".to_string()),
            status: ItemStatus::Translated,
            meta: serde_json::json!({}),
            segments: vec![segment],
        }],
    };
    let initialized = state::initialize(&state_dir, &source, &document, 1_200).unwrap();
    state::write_json_atomic(
        &state::project_dir(&state_dir, &initialized.project.id).join("analysis.json"),
        &test_analysis(),
    )
    .unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();

    retranslate_project(
        Some(config_path),
        source,
        vec!["segment-1".to_string()],
        true,
        Some(sender),
    )
    .await
    .unwrap();

    let progress = receiver.recv().await.unwrap();
    assert_eq!(progress.completed, 1);
    assert_eq!(progress.total, 1);
    assert_eq!(progress.succeeded, 1);
    assert_eq!(progress.failed, 0);
    assert_eq!(progress.item_id, "segment-1");
    assert!(receiver.recv().await.is_none());
    fs::remove_dir_all(dir).unwrap();
}

fn test_analysis() -> BookAnalysis {
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

fn test_segment(id: &str, source: &str) -> Segment {
    Segment {
        id: id.to_string(),
        ordinal: 0,
        source: source.to_string(),
        target: None,
        target_before_polish: None,
        polish_status: None,
        kind: SegmentKind::Paragraph,
        status: ItemStatus::Pending,
        source_hash: "hash".to_string(),
        meta: serde_json::json!({}),
    }
}

struct ExtractionFailingClient;

#[async_trait]
impl TranslationClient for ExtractionFailingClient {
    async fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionOutput, String> {
        if system_prompt.contains("TASK:TERM_EXTRACTION") {
            return Err("request_failed stage=term_extraction: provider unavailable".to_string());
        }
        let value: serde_json::Value =
            serde_json::from_str(user_prompt).expect("prompt should be JSON");
        let segments = value["segments"]
            .as_array()
            .expect("prompt should contain segments");
        Ok(output(
            serde_json::json!({
                "translations": segments.iter().map(|segment| format!("translated {}", segment["source"].as_str().unwrap_or_default())).collect::<Vec<_>>()
            })
            .to_string(),
        ))
    }
}

#[tokio::test]
async fn extraction_failure_does_not_fail_translation() {
    let dir = temp_dir();
    let source = dir.join("book.txt");
    fs::write(&source, "book").expect("source should be written");
    let state_dir = dir.join("projects");
    let document = Document {
        metadata: DocumentMetadata {
            title: "Book".to_string(),
            source_language: "en".to_string(),
            target_language: "zh-CN".to_string(),
            source_format: "txt".to_string(),
        },
        chapters: vec![crate::model::Chapter {
            id: "chapter-1".to_string(),
            title: "Opening".to_string(),
            target_title: None,
            status: ItemStatus::Pending,
            meta: serde_json::json!({ "source_digest": "digest" }),
            segments: vec![test_segment("one", "First paragraph.")],
        }],
    };
    let initialized = state::initialize(&state_dir, &source, &document, 1_200)
        .expect("project should initialize");
    let mut project = initialized.project;
    let mut chapters = state::load_chapters(&state_dir, &project).expect("chapters should load");
    let store = TermStore::open(state::project_dir(&state_dir, &project.id).join("terms.db"))
        .expect("term store should open");
    let analysis = BookAnalysis {
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
    };
    let mut config = AppConfig::default();
    config.llm.max_retries = 0;

    run_transit(
        Arc::new(ExtractionFailingClient),
        &store,
        &state_dir,
        &mut project,
        &mut chapters,
        &analysis,
        &config,
        None,
    )
    .await
    .expect("translation should succeed despite extraction failure");

    assert!(chapters[0]
        .segments
        .iter()
        .all(|segment| segment.status == ItemStatus::Translated));
    let log = fs::read_to_string(state::project_dir(&state_dir, &project.id).join("logs.txt"))
        .expect("logs should be readable");
    assert!(log.contains("term_extraction_failed"));
    // The failed extraction stays pending so a later run can retry it.
    assert!(store
        .pending_extractions()
        .expect("pending extractions should list")
        .iter()
        .any(|extraction| extraction.chapter_id == "chapter-1"));
    drop(store);
    fs::remove_dir_all(dir).expect("temp directory should be removed");
}
