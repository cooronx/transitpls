//! 术语抽取的排队与重试。
//!
//! 翻译过程中先把待抽取批次写入章节 `meta` 和术语库，抽取成功后再清除；
//! 中断后由 `retry_pending_extractions` 在下次运行时补做。

use super::common::chapter_body_complete;
use crate::config::AppConfig;
use crate::llm::TranslationClient;
use crate::model::{Chapter, ProjectState};
use crate::pipeline;
use crate::state;
use crate::terms::{self, PendingExtraction, Term, TermStore};
use std::path::Path;

/// 章节正文完成后抽取一次整章术语，每章只做一次。
pub(super) async fn extract_completed_chapter<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    state_dir: &Path,
    project: &mut ProjectState,
    chapters: &mut [Chapter],
    chapter_index: usize,
    config: &AppConfig,
) -> Result<(), String> {
    if !chapter_body_complete(&chapters[chapter_index])
        || chapters[chapter_index].meta["terms_extracted"] == serde_json::Value::Bool(true)
    {
        return Ok(());
    }
    let extraction = PendingExtraction {
        chapter_id: chapters[chapter_index].id.clone(),
        batch_key: "__chapter__".to_string(),
        source_text: chapters[chapter_index]
            .segments
            .iter()
            .map(|segment| segment.source.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        target_text: chapters[chapter_index]
            .segments
            .iter()
            .filter_map(|segment| segment.target.as_deref())
            .collect::<Vec<_>>()
            .join("\n"),
    };
    record_pending_extraction(&mut chapters[chapter_index], &extraction)?;
    state::write_chapter(state_dir, project, &chapters[chapter_index])?;
    pipeline::write_context(
        &state::project_dir(state_dir, &project.id),
        chapters,
        chapter_index,
        chapters[chapter_index].segments.len(),
        config.pipeline.recent_context_chars,
    )?;
    store.queue_extraction(&extraction)?;
    if let Err(error) = process_extraction(
        client,
        store,
        &extraction,
        chapter_index,
        config.llm.max_retries,
    )
    .await
    {
        // Term extraction is auxiliary: keep the translated chapter and retry
        // the pending extraction on a later run.
        state::append_log(
            state_dir,
            project,
            "term_extraction_failed",
            serde_json::json!({ "chapter_id": extraction.chapter_id, "error": error }),
        )?;
        return Ok(());
    }
    clear_pending_extraction(&mut chapters[chapter_index], &extraction.batch_key);
    chapters[chapter_index].meta["terms_extracted"] = serde_json::Value::Bool(true);
    state::write_chapter(state_dir, project, &chapters[chapter_index])
}

/// 补做所有待抽取任务：章节 `meta` 中的记录先补写进术语库，再统一执行。
pub(super) async fn retry_pending_extractions<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    state_dir: &Path,
    project: &ProjectState,
    chapters: &mut [Chapter],
    max_retries: usize,
    recent_context_chars: usize,
) -> Result<(), String> {
    for chapter in chapters.iter() {
        if let Some(pending) = chapter
            .meta
            .get("pending_term_extractions")
            .and_then(serde_json::Value::as_object)
        {
            for value in pending.values() {
                let extraction: PendingExtraction = serde_json::from_value(value.clone())
                    .map_err(|error| format!("invalid pending term extraction state: {error}"))?;
                store.queue_extraction(&extraction)?;
            }
        }
    }
    for mut extraction in store.pending_extractions()? {
        let chapter_index = chapters
            .iter()
            .position(|chapter| chapter.id == extraction.chapter_id)
            .ok_or_else(|| {
                format!(
                    "pending term extraction references missing chapter {}",
                    extraction.chapter_id
                )
            })?;
        // 断点中的文本可能早于人工编辑；重试时使用当前保存的译文作为术语证据。
        refresh_extraction_target(&mut extraction, &chapters[chapter_index]);
        let before_segment = if extraction.batch_key == "__chapter__" {
            chapters[chapter_index].segments.len()
        } else {
            extraction
                .batch_key
                .split('|')
                .filter_map(|id| {
                    chapters[chapter_index]
                        .segments
                        .iter()
                        .position(|segment| segment.id == id)
                })
                .max()
                .map(|index| index + 1)
                .unwrap_or_default()
        };
        pipeline::write_context(
            &state::project_dir(state_dir, &project.id),
            chapters,
            chapter_index,
            before_segment,
            recent_context_chars,
        )?;
        match process_extraction(client, store, &extraction, chapter_index, max_retries).await {
            Ok(()) => {
                clear_pending_extraction(&mut chapters[chapter_index], &extraction.batch_key);
                if extraction.batch_key == "__chapter__" {
                    chapters[chapter_index].meta["terms_extracted"] = serde_json::Value::Bool(true);
                }
                state::write_chapter(state_dir, project, &chapters[chapter_index])?;
            }
            // Leave the extraction pending and keep translating; a later run
            // retries it instead of failing the whole task.
            Err(error) => {
                state::append_log(
                    state_dir,
                    project,
                    "term_extraction_failed",
                    serde_json::json!({
                        "chapter_id": extraction.chapter_id,
                        "batch_key": extraction.batch_key,
                        "error": error,
                    }),
                )?;
            }
        }
    }
    Ok(())
}

/// 把待抽取任务记录到章节 `meta`，供中断恢复。
pub(super) fn record_pending_extraction(
    chapter: &mut Chapter,
    extraction: &PendingExtraction,
) -> Result<(), String> {
    if !chapter.meta.is_object() {
        chapter.meta = serde_json::json!({});
    }
    let pending = chapter
        .meta
        .as_object_mut()
        .expect("chapter meta was normalized to an object")
        .entry("pending_term_extractions")
        .or_insert_with(|| serde_json::json!({}));
    let object = pending
        .as_object_mut()
        .ok_or_else(|| "chapter pending_term_extractions must be an object".to_string())?;
    object.insert(
        extraction.batch_key.clone(),
        serde_json::to_value(extraction)
            .map_err(|error| format!("failed to store pending extraction: {error}"))?,
    );
    Ok(())
}

/// 抽取完成后从章节 `meta` 移除记录，全部清空时删除整个字段。
pub(super) fn clear_pending_extraction(chapter: &mut Chapter, batch_key: &str) {
    let Some(meta) = chapter.meta.as_object_mut() else {
        return;
    };
    let should_remove = meta
        .get_mut("pending_term_extractions")
        .and_then(serde_json::Value::as_object_mut)
        .is_some_and(|pending| {
            pending.remove(batch_key);
            pending.is_empty()
        });
    if should_remove {
        meta.remove("pending_term_extractions");
    }
}

pub(super) async fn process_extraction<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    extraction: &PendingExtraction,
    chapter_index: usize,
    max_retries: usize,
) -> Result<(), String> {
    let known_terms = store.relevant(&extraction.source_text)?;
    let extracted = terms::extract_terms_resilient(
        client,
        &extraction.source_text,
        &extraction.target_text,
        &known_terms,
        chapter_index,
        max_retries,
    )
    .await?;
    store_extraction(store, extraction, &extracted)
}

/// 把已抽取的术语写入术语库并标记批次完成；并发模式下由协调者调用。
pub(super) fn store_extraction(
    store: &TermStore,
    extraction: &PendingExtraction,
    extracted: &[Term],
) -> Result<(), String> {
    for term in extracted {
        store.insert_with_evidence(term, &extraction.source_text, &extraction.target_text)?;
    }
    store.complete_extraction(&extraction.chapter_id, &extraction.batch_key)
}

fn refresh_extraction_target(extraction: &mut PendingExtraction, chapter: &Chapter) {
    let key = extraction
        .batch_key
        .strip_prefix("retranslate:")
        .unwrap_or(&extraction.batch_key);
    if key.strip_prefix("title:") == Some(chapter.id.as_str()) {
        if let Some(target) = &chapter.target_title {
            extraction.target_text = target.clone();
        }
        return;
    }
    let segments = if key == "__chapter__" {
        chapter.segments.iter().collect::<Vec<_>>()
    } else {
        key.split('|')
            .filter_map(|id| chapter.segments.iter().find(|segment| segment.id == id))
            .collect()
    };
    if !segments.is_empty() && segments.iter().all(|segment| segment.target.is_some()) {
        extraction.target_text = segments
            .iter()
            .filter_map(|segment| segment.target.as_deref())
            .collect::<Vec<_>>()
            .join("\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_extractions_use_current_saved_text_after_editing() {
        let chapter: Chapter = serde_json::from_value(serde_json::json!({
            "id":"c", "title":"title", "target_title":"人工标题", "status":"translated", "meta":{},
            "segments":[{"id":"s", "ordinal":0, "source":"source", "target":"人工译文", "kind":"paragraph", "status":"translated", "source_hash":"hash", "meta":{}}]
        })).unwrap();
        for (key, expected) in [
            ("s", "人工译文"),
            ("retranslate:s", "人工译文"),
            ("__chapter__", "人工译文"),
            ("retranslate:title:c", "人工标题"),
        ] {
            let mut extraction = PendingExtraction {
                chapter_id: "c".into(),
                batch_key: key.into(),
                source_text: "source".into(),
                target_text: "过期译文".into(),
            };
            refresh_extraction_target(&mut extraction, &chapter);
            assert_eq!(extraction.target_text, expected);
        }
    }
}
