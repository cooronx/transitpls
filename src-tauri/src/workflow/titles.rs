//! 章节标题翻译：正文已完成的章节补齐缺失的译文标题，并同步章节内标题段落。

use super::common::{chapter_body_complete, save_project_progress};
use crate::analysis::BookAnalysis;
use crate::config::AppConfig;
use crate::llm::{self, TranslationClient};
use crate::model::{Chapter, ItemStatus, ProjectState, Segment, SegmentKind};
use crate::pipeline;
use crate::state;
use crate::terms::TermStore;
use std::path::Path;

/// 翻译正文已完成的章节中缺失的标题。
///
/// 先按批次翻译；整批失败时回退到逐章翻译。已存在的标题会同步到章节内
/// 与标题同名的标题段落，保证正文与目录一致。
pub(super) async fn translate_missing_titles<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    state_dir: &Path,
    project: &mut ProjectState,
    chapters: &mut [Chapter],
    analysis: &BookAnalysis,
    config: &AppConfig,
) -> Result<(), String> {
    for chapter in chapters.iter_mut() {
        if let Some(target_title) = chapter.target_title.clone() {
            sync_heading_title(chapter, &target_title)?;
        }
    }
    let title_segments = chapters
        .iter()
        .map(|chapter| Segment {
            id: chapter.id.clone(),
            ordinal: 0,
            source: chapter.title.clone(),
            target: chapter.target_title.clone(),
            target_before_polish: None,
            polish_status: None,
            kind: SegmentKind::Heading,
            status: if chapter.target_title.is_some() {
                ItemStatus::Translated
            } else {
                ItemStatus::Pending
            },
            source_hash: String::new(),
            meta: serde_json::json!({}),
        })
        .collect::<Vec<_>>();
    for range in pipeline::batch_ranges(&title_segments, config.segment.max_chars_per_batch) {
        let indices = range
            .filter(|&index| {
                chapters[index].target_title.is_none() && chapter_body_complete(&chapters[index])
            })
            .collect::<Vec<_>>();
        if indices.is_empty() {
            continue;
        }
        let request = indices
            .iter()
            .map(|&index| title_segments[index].clone())
            .collect::<Vec<_>>();
        let relevant_terms = store.relevant(
            &request
                .iter()
                .map(|title| title.source.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        )?;
        match llm::translate_titles(
            client,
            &request,
            &project.source_language,
            &project.target_language,
            &analysis.style_guide,
            &relevant_terms,
            config.llm.max_retries,
        )
        .await
        {
            Ok(translations) => {
                for (&index, translation) in indices.iter().zip(translations) {
                    apply_title(
                        state_dir,
                        project,
                        chapters,
                        index,
                        translation,
                        crate::revisions::RevisionKind::Translation,
                        client.model_name(),
                    )?;
                }
            }
            Err(_) => {
                for &index in &indices {
                    let translation = llm::translate_titles(
                        client,
                        std::slice::from_ref(&title_segments[index]),
                        &project.source_language,
                        &project.target_language,
                        &analysis.style_guide,
                        &store.relevant(&title_segments[index].source)?,
                        config.llm.max_retries,
                    )
                    .await
                    .map_err(|error| {
                        format!("chapter {} title failed: {error}", chapters[index].id)
                    })?
                    .remove(0);
                    apply_title(
                        state_dir,
                        project,
                        chapters,
                        index,
                        translation,
                        crate::revisions::RevisionKind::Translation,
                        client.model_name(),
                    )?;
                }
            }
        }
    }
    save_project_progress(state_dir, project, chapters)
}

/// 写入章节译文标题，并同步同名的标题段落。
pub(super) fn apply_title(
    state_dir: &Path,
    project: &mut ProjectState,
    chapters: &mut [Chapter],
    chapter_index: usize,
    translation: String,
    kind: crate::revisions::RevisionKind,
    model: Option<&str>,
) -> Result<(), String> {
    crate::revisions::sync_title(&mut chapters[chapter_index], &translation, kind, model)?;
    state::write_chapter(state_dir, project, &chapters[chapter_index])?;
    save_project_progress(state_dir, project, chapters)
}

/// 把章节标题同步到正文中与章节同名的标题段落。
pub(super) fn sync_heading_title(chapter: &mut Chapter, target_title: &str) -> Result<(), String> {
    crate::revisions::sync_title(
        chapter,
        target_title,
        crate::revisions::RevisionKind::Translation,
        None,
    )
}
