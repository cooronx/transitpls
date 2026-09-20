use super::*;
use crate::model::{Chapter, Document, DocumentMetadata, ItemStatus, ProjectState};
use std::path::PathBuf;

struct Fixture {
    dir: PathBuf,
    project: ProjectState,
}
impl Fixture {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("transitpls-edit-{:032x}", rand::random::<u128>()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.txt");
        std::fs::write(&source, "Chapter 1\n\nsource").unwrap();
        let document = Document {
            metadata: DocumentMetadata { title: "Book".into(), source_language: "en".into(), target_language: "zh-CN".into(), source_format: "txt".into() },
            chapters: vec![Chapter { id:"chapter-1".into(), title:"Chapter 1".into(), target_title:Some("第一章".into()), status:ItemStatus::Translated, meta:serde_json::json!({}), segments:vec![serde_json::from_value(serde_json::json!({
                "id":"s", "ordinal":0, "source":"source", "target":"旧译文", "target_before_polish":"草稿",
                "kind":"paragraph", "status":"translated", "source_hash":"hash", "meta":{}
            })).unwrap()] }],
        };
        let project = state::initialize(&dir, &source, &document, 1200)
            .unwrap()
            .project;
        Self { dir, project }
    }
    fn read(&self) -> SegmentDetail {
        read_segment(&self.dir, &self.project.id, "s").unwrap()
    }
    fn request(&self, detail: &SegmentDetail, edit: Edit) -> EditRequest {
        EditRequest {
            project_id: self.project.id.clone(),
            segment_id: "s".into(),
            expected_revision: detail.revision,
            expected_target: detail.segment.target.clone().unwrap(),
            edit,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

#[test]
fn edits_are_atomic_with_history_and_project_lock_and_do_not_resurrect_old_drafts() {
    let fixture = Fixture::new();
    let old = fixture.read();
    let lock = state::acquire_project_lock(&fixture.dir, &fixture.project).unwrap();
    assert!(save_segment(
        &fixture.dir,
        fixture.request(
            &old,
            Edit::Manual {
                target: "阻止写入".into()
            }
        )
    )
    .is_err());
    assert_eq!(fixture.read().segment.target, old.segment.target);
    drop(lock);
    let saved = save_segment(
        &fixture.dir,
        fixture.request(
            &old,
            Edit::Manual {
                target: "人工修正".into(),
            },
        ),
    )
    .unwrap();
    let reloaded = fixture.read();
    assert_eq!(saved.revision, reloaded.revision);
    assert_eq!(
        reloaded.segment.target_before_polish.as_deref(),
        Some("人工修正")
    );
    assert!(revisions::is_protected(&reloaded.segment));
    assert!(crate::polish::plan_batches(
        "round",
        &state::load_chapters(&fixture.dir, &fixture.project).unwrap(),
        1800
    )
    .is_empty());
    assert_eq!(
        crate::polish::pending_segment_count(
            &state::load_chapters(&fixture.dir, &fixture.project).unwrap()
        ),
        0
    );
    assert!(save_segment(
        &fixture.dir,
        fixture.request(
            &old,
            Edit::Manual {
                target: "过期修改".into()
            }
        )
    )
    .is_err());
    let restored = save_segment(
        &fixture.dir,
        fixture.request(
            &saved,
            Edit::Restore {
                revision_id: old.revision,
            },
        ),
    )
    .unwrap();
    assert_eq!(restored.segment.target, old.segment.target);
    assert_eq!(restored.revisions.len(), saved.revisions.len() + 1);
    assert!(restored
        .revisions
        .iter()
        .any(|revision| revision.target == "人工修正"));
    let unchanged = save_segment(
        &fixture.dir,
        fixture.request(
            &restored,
            Edit::Manual {
                target: restored.segment.target.clone().unwrap(),
            },
        ),
    )
    .unwrap();
    assert_eq!(unchanged.revisions.len(), restored.revisions.len());
}

#[test]
fn preview_is_separate_from_current_translation_and_cannot_be_adopted_after_an_edit() {
    let fixture = Fixture::new();
    let old = fixture.read();
    let preview = RetranslationPreview {
        id: "preview".into(),
        target: "模型建议".into(),
        base_revision: old.revision,
        base_target: old.segment.target.clone().unwrap(),
        model: Some("actual-model".into()),
    };
    let mut chapters = state::load_chapters(&fixture.dir, &fixture.project).unwrap();
    chapters[0].segments[0].meta["retranslation_preview"] = serde_json::to_value(&preview).unwrap();
    state::write_chapter(&fixture.dir, &fixture.project, &chapters[0]).unwrap();
    assert_eq!(fixture.read().segment.target, old.segment.target);
    let adopted = save_segment(
        &fixture.dir,
        fixture.request(
            &old,
            Edit::Adopt {
                preview_id: "preview".into(),
            },
        ),
    )
    .unwrap();
    assert_eq!(adopted.segment.target.as_deref(), Some("模型建议"));
    assert_eq!(
        adopted.revisions.last().unwrap().model.as_deref(),
        Some("actual-model")
    );
    assert!(adopted.preview.is_none());
    assert!(revisions::is_protected(&adopted.segment));
    assert!(save_segment(
        &fixture.dir,
        fixture.request(
            &old,
            Edit::Adopt {
                preview_id: "preview".into()
            }
        )
    )
    .is_err());
    let mut chapters = state::load_chapters(&fixture.dir, &fixture.project).unwrap();
    chapters[0].segments[0].meta["retranslation_preview"] = serde_json::to_value(preview).unwrap();
    state::write_chapter(&fixture.dir, &fixture.project, &chapters[0]).unwrap();
    assert!(save_segment(
        &fixture.dir,
        fixture.request(
            &adopted,
            Edit::Adopt {
                preview_id: "preview".into()
            }
        )
    )
    .is_err());
}

#[test]
fn editing_a_chapter_heading_updates_the_export_title() {
    let fixture = Fixture::new();
    let mut chapters = state::load_chapters(&fixture.dir, &fixture.project).unwrap();
    chapters[0].segments[0].kind = SegmentKind::Heading;
    chapters[0].segments[0].source = "Chapter 1".into();
    state::write_chapter(&fixture.dir, &fixture.project, &chapters[0]).unwrap();
    save_segment(
        &fixture.dir,
        fixture.request(
            &fixture.read(),
            Edit::Manual {
                target: "序章".into(),
            },
        ),
    )
    .unwrap();
    assert_eq!(
        state::load_chapters(&fixture.dir, &fixture.project).unwrap()[0]
            .target_title
            .as_deref(),
        Some("序章")
    );
}

#[tokio::test]
async fn batch_title_retranslation_cannot_overwrite_a_manual_heading() {
    let fixture = Fixture::new();
    let mut project = fixture.project.clone();
    let mut chapters = state::load_chapters(&fixture.dir, &project).unwrap();
    chapters[0].segments[0].kind = SegmentKind::Heading;
    chapters[0].segments[0].source = chapters[0].title.clone();
    revisions::sync_title(&mut chapters[0], "人工标题", RevisionKind::Manual, None).unwrap();
    let analysis: crate::analysis::BookAnalysis = serde_json::from_value(serde_json::json!({
        "genre":"fiction", "tone":"neutral", "style_guide":[], "narration":"third person", "pacing":"steady", "register":"neutral", "dialogue_style":"plain", "rhetoric":"plain", "characters":[], "terms":[], "book_synopsis":"synopsis"
    })).unwrap();
    let store = term_store(&fixture.dir, &project).unwrap();
    let error = super::super::retranslate::retranslate_item(
        &crate::llm::MockClient,
        &store,
        &fixture.dir,
        &mut project,
        &mut chapters,
        &analysis,
        &AppConfig::default(),
        "title:chapter-1",
    )
    .await
    .unwrap_err();
    assert!(error.contains("人工修改"));
    assert_eq!(chapters[0].segments[0].target.as_deref(), Some("人工标题"));
    assert_eq!(chapters[0].target_title.as_deref(), Some("人工标题"));
    drop(store);
}

#[tokio::test]
async fn generating_a_preview_preserves_the_current_translation_and_history() {
    let fixture = Fixture::new();
    let mut project = fixture.project.clone();
    let mut chapters = state::load_chapters(&fixture.dir, &project).unwrap();
    crate::analysis::prepare(
        &crate::llm::MockClient,
        &fixture.dir,
        &mut project,
        &mut chapters,
        true,
        false,
        0,
    )
    .await
    .unwrap();
    let old = fixture.read();
    let mut config = AppConfig::default();
    config.analysis.full_book = false;
    let generated = preview_with_client(
        &fixture.dir,
        &config,
        &project,
        "s",
        &crate::llm::MockClient,
    )
    .await
    .unwrap();
    assert_eq!(generated.segment.target, old.segment.target);
    assert_eq!(generated.revisions, old.revisions);
    let preview = generated.preview.unwrap();
    assert_ne!(Some(&preview.target), old.segment.target.as_ref());
    assert_eq!(preview.model.as_deref(), Some("mock"));
    assert_eq!(preview.base_revision, old.revision);
    assert_eq!(fixture.read().preview.unwrap().id, preview.id);
    assert!(!revisions::is_protected(&fixture.read().segment));
}
