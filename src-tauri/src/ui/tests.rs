//! ui 模块的单元测试：凭据状态、日志读取、项目详情、恢复译文与影响扫描。

use super::dto::ProgressSnapshot;
use super::project::{credential_status, delete_project_at, project_detail, read_logs};
use super::terms::{restore_translation, scan_term_impact};
use crate::model::{Chapter, Document, DocumentMetadata, ItemStatus, Segment, SegmentKind};
use crate::state;
use serde_json::Value;
use std::fs;

#[test]
fn local_no_key_configuration_is_ready_for_desktop_tasks() {
    let mut config = crate::config::AppConfig::default();
    config.llm.provider = "openai-compatible".into();
    config.llm.base_url = Some("http://localhost:11434/v1".into());
    config.llm.api_key_env.clear();
    let status = credential_status(&config).unwrap();
    assert!(status.configured);
    assert_eq!(status.source, Some("none"));
    let serialized = serde_json::to_string(&status).unwrap();
    assert!(!serialized.contains("lastFour"));
}

#[test]
fn reads_newest_logs_first() {
    let path = std::env::temp_dir().join(format!(
        "transitpls-ui-log-{}-{}.txt",
        std::process::id(),
        std::thread::current()
            .name()
            .unwrap_or("test")
            .replace(':', "_")
    ));
    fs::write(
        &path,
        "2026-01-01T00:00:00Z\tstarted\t{}\n2026-01-01T00:00:01Z\tdone\t{\"count\":1}\n",
    )
    .expect("fixture should be written");
    let logs = read_logs(&path).expect("logs should parse");
    assert_eq!(logs[0].event, "done");
    assert_eq!(logs[1].event, "started");
    fs::remove_file(path).expect("fixture should be removed");
}

#[test]
fn project_detail_reads_the_same_state_as_the_cli() {
    let root = std::env::temp_dir().join(format!(
        "transitpls-ui-project-{}-{}",
        std::process::id(),
        std::thread::current()
            .name()
            .unwrap_or("test")
            .replace(':', "_")
    ));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let input = root.join("book.txt");
    fs::write(&input, "Hello world").expect("source should be written");
    let document = Document {
        metadata: DocumentMetadata {
            title: "Book".to_string(),
            source_language: "en".to_string(),
            target_language: "zh-CN".to_string(),
            source_format: "txt".to_string(),
        },
        chapters: vec![Chapter {
            id: "chapter-0000-book".to_string(),
            title: "Book".to_string(),
            target_title: None,
            status: ItemStatus::Pending,
            meta: Value::Null,
            segments: vec![Segment {
                id: "segment-0000".to_string(),
                ordinal: 0,
                source: "Hello world".to_string(),
                target: None,
                target_before_polish: None,
                polish_status: None,
                kind: SegmentKind::Paragraph,
                status: ItemStatus::Pending,
                source_hash: "fixture".to_string(),
                meta: Value::Null,
            }],
        }],
    };
    let initialized =
        state::initialize(&root, &input, &document, 1_200).expect("project should initialize");

    let mut detail = project_detail(&root, &initialized.project.id)
        .expect("desktop bridge should load project state");

    assert_eq!(detail.project.title, "Book");
    assert_eq!(detail.chapters[0].segments[0].source, "Hello world");
    assert_eq!(detail.logs[0].event, "initialized");
    assert!(!detail.task_initialized);

    state::append_log(
        &root,
        &initialized.project,
        "analysis_completed",
        serde_json::json!({}),
    )
    .expect("analysis completion should be recorded");
    detail = project_detail(&root, &initialized.project.id)
        .expect("desktop bridge should reload initialization state");
    assert!(detail.task_initialized);

    let pending = ProgressSnapshot::from(&detail);
    detail.chapters[0].segments[0].target = Some("你好，世界".to_string());
    let translated = ProgressSnapshot::from(&detail);
    assert_ne!(pending, translated);

    fs::remove_dir_all(root).expect("fixture should be removed");
}

#[test]
fn restores_the_last_translation_and_keeps_titles_synchronized() {
    let mut chapters = vec![Chapter {
        id: "chapter-1".to_string(),
        title: "Chapter Alice".to_string(),
        target_title: Some("新标题".to_string()),
        status: ItemStatus::Translated,
        meta: serde_json::json!({ "previous_target_title": "旧标题" }),
        segments: vec![Segment {
            id: "heading-1".to_string(),
            ordinal: 0,
            source: "Chapter Alice".to_string(),
            target: Some("新标题".to_string()),
            target_before_polish: None,
            polish_status: None,
            kind: SegmentKind::Heading,
            status: ItemStatus::Translated,
            source_hash: "fixture".to_string(),
            meta: serde_json::json!({}),
        }],
    }];

    assert_eq!(
        restore_translation(&mut chapters, "title:chapter-1").unwrap(),
        0
    );
    assert_eq!(chapters[0].target_title.as_deref(), Some("旧标题"));
    assert_eq!(chapters[0].segments[0].target.as_deref(), Some("旧标题"));
    assert_eq!(chapters[0].meta["previous_target_title"], "新标题");
}

#[test]
fn impact_scan_covers_content_kinds_and_respects_word_boundaries() {
    let segment = |id: &str, source: &str, kind: SegmentKind| Segment {
        id: id.to_string(),
        ordinal: 0,
        source: source.to_string(),
        target: Some("译文".to_string()),
        target_before_polish: None,
        polish_status: None,
        kind,
        status: ItemStatus::Translated,
        source_hash: "fixture".to_string(),
        meta: serde_json::json!({}),
    };
    let chapters = vec![Chapter {
        id: "chapter-1".to_string(),
        title: "A cat story".to_string(),
        target_title: Some("猫的故事".to_string()),
        status: ItemStatus::Translated,
        meta: serde_json::json!({}),
        segments: vec![
            segment("paragraph", "The cat waits.", SegmentKind::Paragraph),
            segment("quote", "\"cat\"", SegmentKind::Quote),
            segment("metadata", "tag: cat", SegmentKind::Metadata),
            segment("false-positive", "concatenate", SegmentKind::Paragraph),
        ],
    }];

    let affected = scan_term_impact(&chapters, "cat");
    assert_eq!(affected.len(), 4);
    assert!(affected.iter().any(|item| item.kind == "title"));
    assert!(!affected.iter().any(|item| item.id == "false-positive"));
}

#[test]
fn deletes_project_state_without_deleting_source_book() {
    let root = std::env::temp_dir().join(format!(
        "transitpls-ui-delete-{}-{}",
        std::process::id(),
        std::thread::current()
            .name()
            .unwrap_or("test")
            .replace(':', "_")
    ));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let input = root.join("book.txt");
    fs::write(&input, "Hello world").expect("source should be written");
    let document = Document {
        metadata: DocumentMetadata {
            title: "Book".to_string(),
            source_language: "en".to_string(),
            target_language: "zh-CN".to_string(),
            source_format: "txt".to_string(),
        },
        chapters: vec![Chapter {
            id: "chapter-0000-book".to_string(),
            title: "Book".to_string(),
            target_title: None,
            status: ItemStatus::Pending,
            meta: Value::Null,
            segments: Vec::new(),
        }],
    };
    let initialized =
        state::initialize(&root, &input, &document, 1_200).expect("project should initialize");

    delete_project_at(&root, &initialized.project.id).expect("project should be deleted");

    assert!(!state::project_dir(&root, &initialized.project.id).exists());
    assert!(input.exists());
    fs::remove_dir_all(root).expect("fixture should be removed");
}
