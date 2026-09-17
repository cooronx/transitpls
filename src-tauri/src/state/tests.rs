//! state 模块的单元测试：哈希、项目初始化、文件锁、旧数据兼容与导出快照校验。

use super::{acquire_project_lock, hash_file, initialize, load_export_snapshot, save_project};
use crate::model::{Chapter, Document, DocumentMetadata, ItemStatus, PolishStatus};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "transitpls-state-{name}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("temp directory should be created");
    path
}

fn document() -> Document {
    Document {
        metadata: DocumentMetadata {
            title: "Book".to_string(),
            source_language: "en".to_string(),
            target_language: "zh-CN".to_string(),
            source_format: "txt".to_string(),
        },
        chapters: vec![Chapter {
            id: "chapter-1-test".to_string(),
            title: "Chapter 1".to_string(),
            target_title: None,
            status: ItemStatus::Pending,
            meta: serde_json::json!({}),
            segments: Vec::new(),
        }],
    }
}

#[test]
fn hashes_source_bytes_with_sha256() {
    let dir = temp_dir("hash");
    let source = dir.join("book.txt");
    fs::write(&source, b"abc").expect("source should be written");
    assert_eq!(
        hash_file(&source).expect("hash should be generated"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    fs::remove_dir_all(dir).expect("temp directory should be removed");
}

#[test]
fn publishes_complete_project_from_creating_directory() {
    let dir = temp_dir("initialize");
    let source = dir.join("book.txt");
    let state_dir = dir.join("projects");
    fs::write(&source, b"content").expect("source should be written");

    let initialized =
        initialize(&state_dir, &source, &document(), 1_200).expect("init should succeed");
    let project_dir = state_dir.join(&initialized.project.id);
    assert!(project_dir.join("project.json").is_file());
    assert!(project_dir.join("chapters/chapter-1-test.json").is_file());
    assert!(project_dir.join("logs.txt").is_file());
    assert!(state_dir.join(".creating").is_dir());
    assert_eq!(
        fs::read_dir(state_dir.join(".creating"))
            .expect("creating directory should be readable")
            .count(),
        0
    );
    fs::remove_dir_all(dir).expect("temp directory should be removed");
}

#[test]
fn project_lock_rejects_concurrent_writer_and_releases_on_drop() {
    let dir = temp_dir("lock");
    let source = dir.join("book.txt");
    let state_dir = dir.join("projects");
    fs::write(&source, b"content").expect("source should be written");
    let initialized =
        initialize(&state_dir, &source, &document(), 1_200).expect("init should succeed");

    let first = acquire_project_lock(&state_dir, &initialized.project)
        .expect("first writer should acquire lock");
    assert!(acquire_project_lock(&state_dir, &initialized.project).is_err());
    drop(first);
    acquire_project_lock(&state_dir, &initialized.project)
        .expect("lock should be released when writer exits");

    fs::remove_dir_all(dir).expect("temp directory should be removed");
}

#[test]
fn normalizes_legacy_polish_state_without_retranslating() {
    let segment = |id: &str, target: Option<&str>, draft: Option<&str>| crate::model::Segment {
        id: id.to_string(),
        ordinal: 0,
        source: "source".to_string(),
        target: target.map(str::to_string),
        target_before_polish: draft.map(str::to_string),
        polish_status: None,
        kind: crate::model::SegmentKind::Paragraph,
        status: if target.is_some() {
            ItemStatus::Translated
        } else {
            ItemStatus::Pending
        },
        source_hash: "hash".to_string(),
        meta: serde_json::json!({}),
    };
    let mut chapters = vec![Chapter {
        id: "chapter-1".to_string(),
        title: "Chapter 1".to_string(),
        target_title: None,
        status: ItemStatus::Pending,
        meta: serde_json::json!({}),
        segments: vec![
            segment("pending-polish", None, Some("初稿")),
            segment("polished", Some("润色稿"), Some("初稿")),
            segment("plain", Some("译文"), None),
            segment("untranslated", None, None),
        ],
    }];

    super::normalize_polish_state(&mut chapters);

    let segments = &chapters[0].segments;
    assert_eq!(segments[0].target.as_deref(), Some("初稿"));
    assert_eq!(segments[0].status, ItemStatus::Translated);
    assert_eq!(segments[0].polish_status, Some(PolishStatus::Pending));
    assert_eq!(segments[1].target.as_deref(), Some("润色稿"));
    assert_eq!(segments[1].polish_status, Some(PolishStatus::Succeeded));
    assert_eq!(segments[2].polish_status, None);
    assert!(segments[3].target.is_none());
    assert!(segments[3].polish_status.is_none());
}

#[test]
fn export_snapshot_reports_changed_source_hash() {
    let dir = temp_dir("export-hash");
    let source = dir.join("book.txt");
    let state_dir = dir.join("projects");
    fs::write(&source, b"original").expect("source should be written");
    let initialized =
        initialize(&state_dir, &source, &document(), 1_200).expect("init should succeed");
    drop(
        acquire_project_lock(&state_dir, &initialized.project)
            .expect("project lock should be created"),
    );
    fs::write(&source, b"changed").expect("source should change");

    let error =
        load_export_snapshot(&state_dir, &source).expect_err("changed source must not be exported");
    assert!(error.contains("source file hash changed"));
    fs::remove_dir_all(dir).expect("temp directory should be removed");
}

#[test]
fn export_snapshot_rejects_project_chapter_count_mismatch() {
    let dir = temp_dir("export-snapshot");
    let source = dir.join("book.txt");
    let state_dir = dir.join("projects");
    fs::write(&source, b"content").expect("source should be written");
    let initialized =
        initialize(&state_dir, &source, &document(), 1_200).expect("init should succeed");
    drop(
        acquire_project_lock(&state_dir, &initialized.project)
            .expect("project lock should be created"),
    );
    let mut project = initialized.project;
    project.chapters_total = 2;
    save_project(&state_dir, &project).expect("project should be updated");

    let error = load_export_snapshot(&state_dir, &source)
        .expect_err("inconsistent snapshot must not be exported");
    assert!(error.contains("declares 2 chapters"));
    fs::remove_dir_all(dir).expect("temp directory should be removed");
}
