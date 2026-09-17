//! 单条重译：并发重译指定段落或章节标题，并同步术语与润色快照。

use super::common::{build_client, load_translation_analysis, save_project_progress, term_store};
use super::extraction::{clear_pending_extraction, record_pending_extraction};
use super::titles::apply_title;
use super::transit::request_translation;
use crate::analysis::BookAnalysis;
use crate::config::{self, AppConfig};
use crate::llm::TranslationClient;
use crate::model::{Chapter, ItemStatus, PolishStatus, ProjectState, Segment, SegmentKind};
use crate::pipeline;
use crate::polish;
use crate::state;
use crate::terms::{self, PendingExtraction, Term, TermStore};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 重译进度事件，界面据此更新进度条。
#[derive(Debug, Clone)]
pub struct RetranslationProgress {
    pub completed: usize,
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub item_id: String,
}

/// 界面入口：根据源文件加载项目并重译指定条目。
pub async fn retranslate_project(
    config_path: Option<PathBuf>,
    input: PathBuf,
    item_ids: Vec<String>,
    mock_client: bool,
    progress: Option<tokio::sync::mpsc::UnboundedSender<RetranslationProgress>>,
) -> Result<ProjectState, String> {
    let loaded = config::load(config_path.as_deref())?;
    let mut project = state::load_for_source(&loaded.state_dir, &input)?;
    let _lock = state::acquire_project_lock(&loaded.state_dir, &project)?;
    let mut chapters = state::load_chapters(&loaded.state_dir, &project)?;
    let analysis =
        load_translation_analysis(&loaded.state_dir, &project, &chapters, &loaded.value)?;
    let client: Arc<dyn TranslationClient> = Arc::from(build_client(
        &loaded.value,
        mock_client,
        &loaded.state_dir,
        &project,
    )?);
    let store = term_store(&loaded.state_dir, &project)?;
    let total = item_ids.len();
    let mut succeeded = 0;
    let mut failed = 0;
    state::append_log(
        &loaded.state_dir,
        &project,
        "retranslation_started",
        serde_json::json!({ "total": total }),
    )?;

    let project_snapshot = Arc::new(project.clone());
    let chapters_snapshot = Arc::new(chapters.clone());
    let analysis = Arc::new(analysis);
    let config = Arc::new(loaded.value);
    let mut pending = item_ids.into_iter();
    let mut tasks = tokio::task::JoinSet::new();
    let concurrency = config.general.retranslation_concurrency.min(total);
    for _ in 0..concurrency {
        if let Some(item_id) = pending.next() {
            spawn_retranslation(
                &mut tasks,
                Arc::clone(&client),
                store.clone(),
                Arc::clone(&project_snapshot),
                Arc::clone(&chapters_snapshot),
                Arc::clone(&analysis),
                Arc::clone(&config),
                item_id,
            );
        }
    }

    while let Some(result) = tasks.join_next().await {
        let (item_id, prepared) =
            result.map_err(|error| format!("retranslation worker failed: {error}"))?;
        let result = prepared.and_then(|prepared| {
            apply_retranslation(
                &store,
                &loaded.state_dir,
                &mut project,
                &mut chapters,
                prepared,
            )
        });
        match result {
            Ok(()) => succeeded += 1,
            Err(error) => {
                failed += 1;
                mark_retranslation_error(
                    &loaded.state_dir,
                    &project,
                    &mut chapters,
                    &item_id,
                    &error,
                )?;
                state::append_log(
                    &loaded.state_dir,
                    &project,
                    "retranslation_failed",
                    serde_json::json!({ "item_id": item_id, "error": error }),
                )?;
            }
        }
        if let Some(progress) = &progress {
            let _ = progress.send(RetranslationProgress {
                completed: succeeded + failed,
                total,
                succeeded,
                failed,
                item_id: item_id.clone(),
            });
        }
        if let Some(next_item_id) = pending.next() {
            spawn_retranslation(
                &mut tasks,
                Arc::clone(&client),
                store.clone(),
                Arc::clone(&project_snapshot),
                Arc::clone(&chapters_snapshot),
                Arc::clone(&analysis),
                Arc::clone(&config),
                next_item_id,
            );
        }
    }
    save_project_progress(&loaded.state_dir, &mut project, &chapters)?;
    state::append_log(
        &loaded.state_dir,
        &project,
        "retranslation_completed",
        serde_json::json!({ "total": total, "succeeded": succeeded, "failed": failed }),
    )?;
    Ok(project)
}

#[allow(clippy::too_many_arguments)]
fn spawn_retranslation(
    tasks: &mut tokio::task::JoinSet<(String, Result<PreparedRetranslation, String>)>,
    client: Arc<dyn TranslationClient>,
    store: TermStore,
    project: Arc<ProjectState>,
    chapters: Arc<Vec<Chapter>>,
    analysis: Arc<BookAnalysis>,
    config: Arc<AppConfig>,
    item_id: String,
) {
    tasks.spawn(async move {
        let result = prepare_retranslation(
            client.as_ref(),
            &store,
            &project,
            &chapters,
            &analysis,
            &config,
            &item_id,
        )
        .await;
        (item_id, result)
    });
}

/// 一次重译请求的准备结果：目标位置、原文、新译文与抽取出的术语。
struct PreparedRetranslation {
    item_id: String,
    chapter_index: usize,
    segment_index: Option<usize>,
    source: String,
    draft: Option<String>,
    target: String,
    extracted: Result<Vec<Term>, String>,
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(crate) async fn retranslate_item<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    state_dir: &Path,
    project: &mut ProjectState,
    chapters: &mut [Chapter],
    analysis: &BookAnalysis,
    config: &AppConfig,
    item_id: &str,
) -> Result<(), String> {
    let prepared =
        prepare_retranslation(client, store, project, chapters, analysis, config, item_id).await?;
    apply_retranslation(store, state_dir, project, chapters, prepared)
}

#[allow(clippy::too_many_arguments)]
async fn prepare_retranslation<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    project: &ProjectState,
    chapters: &[Chapter],
    analysis: &BookAnalysis,
    config: &AppConfig,
    item_id: &str,
) -> Result<PreparedRetranslation, String> {
    // 标题条目的 ID 形如 `title:<chapter_id>`。
    if let Some(chapter_id) = item_id.strip_prefix("title:") {
        let chapter_index = chapters
            .iter()
            .position(|chapter| chapter.id == chapter_id)
            .ok_or_else(|| format!("chapter was removed: {chapter_id}"))?;
        let request = Segment {
            id: item_id.to_string(),
            ordinal: 0,
            source: chapters[chapter_index].title.clone(),
            target: chapters[chapter_index].target_title.clone(),
            target_before_polish: None,
            polish_status: None,
            kind: SegmentKind::Heading,
            status: ItemStatus::Translated,
            source_hash: String::new(),
            meta: serde_json::json!({}),
        };
        let translation = crate::llm::translate_titles(
            client,
            &[request],
            &project.source_language,
            &project.target_language,
            &analysis.style_guide,
            &store.relevant(&chapters[chapter_index].title)?,
            config.llm.max_retries,
        )
        .await?
        .remove(0);
        let source = chapters[chapter_index].title.clone();
        let extracted = terms::extract_terms_resilient(
            client,
            &source,
            &translation,
            chapter_index,
            config.llm.max_retries,
        )
        .await;
        return Ok(PreparedRetranslation {
            item_id: item_id.to_string(),
            chapter_index,
            segment_index: None,
            source,
            draft: None,
            target: translation,
            extracted,
        });
    }

    let (chapter_index, segment_index) = chapters
        .iter()
        .enumerate()
        .find_map(|(chapter_index, chapter)| {
            chapter
                .segments
                .iter()
                .position(|segment| segment.id == item_id)
                .map(|segment_index| (chapter_index, segment_index))
        })
        .ok_or_else(|| format!("content was removed: {item_id}"))?;
    let source = chapters[chapter_index].segments[segment_index]
        .source
        .clone();
    let relevant_terms = store.relevant(&source)?;
    let digest = chapters[chapter_index]
        .meta
        .get("source_digest")
        .and_then(serde_json::Value::as_str);
    let recent = pipeline::recent_targets(
        chapters,
        chapter_index,
        segment_index,
        config.pipeline.recent_context_chars,
    );
    let request = chapters[chapter_index].segments[segment_index].clone();
    let draft = request_translation(
        client,
        std::slice::from_ref(&request),
        project,
        analysis,
        digest,
        &relevant_terms,
        &recent,
        config.llm.max_retries,
    )
    .await?
    .remove(0);

    let extracted = terms::extract_terms_resilient(
        client,
        &source,
        &draft,
        chapter_index,
        config.llm.max_retries,
    )
    .await;
    Ok(PreparedRetranslation {
        item_id: item_id.to_string(),
        chapter_index,
        segment_index: Some(segment_index),
        source,
        draft: Some(draft.clone()),
        target: draft,
        extracted,
    })
}

/// 应用重译结果：保留旧译文到 `previous_target`，写入新草稿并同步术语。
fn apply_retranslation(
    store: &TermStore,
    state_dir: &Path,
    project: &mut ProjectState,
    chapters: &mut [Chapter],
    prepared: PreparedRetranslation,
) -> Result<(), String> {
    let chapter_index = prepared.chapter_index;
    if let Some(segment_index) = prepared.segment_index {
        let segment = &mut chapters[chapter_index].segments[segment_index];
        if !segment.meta.is_object() {
            segment.meta = serde_json::json!({});
        }
        if let Some(previous) = segment.target.clone() {
            segment.meta["previous_target"] = serde_json::Value::String(previous);
        }
        segment
            .meta
            .as_object_mut()
            .unwrap()
            .remove("retranslation_error");
        segment.target_before_polish = prepared.draft;
        segment.target = Some(prepared.target.clone());
        segment.polish_status = Some(PolishStatus::Pending);
        segment.status = ItemStatus::Translated;
        // 标题段落跟随章节标题一起更新。
        if segment.kind == SegmentKind::Heading
            && segment.source.trim() == chapters[chapter_index].title.trim()
        {
            if let Some(previous) = chapters[chapter_index].target_title.clone() {
                chapters[chapter_index].meta["previous_target_title"] =
                    serde_json::Value::String(previous);
            }
            chapters[chapter_index].target_title = Some(prepared.target.clone());
        }
        state::write_chapter(state_dir, project, &chapters[chapter_index])?;
    } else {
        if !chapters[chapter_index].meta.is_object() {
            chapters[chapter_index].meta = serde_json::json!({});
        }
        if let Some(previous) = chapters[chapter_index].target_title.clone() {
            chapters[chapter_index].meta["previous_target_title"] =
                serde_json::Value::String(previous);
        }
        chapters[chapter_index]
            .meta
            .as_object_mut()
            .unwrap()
            .remove("retranslation_error");
        apply_title(
            state_dir,
            project,
            chapters,
            chapter_index,
            prepared.target.clone(),
        )?;
    }

    let extraction = PendingExtraction {
        chapter_id: chapters[chapter_index].id.clone(),
        batch_key: format!("retranslate:{}", prepared.item_id),
        source_text: prepared.source,
        target_text: prepared.target,
    };
    record_pending_extraction(&mut chapters[chapter_index], &extraction)?;
    store.queue_extraction(&extraction)?;
    if let Ok(extracted) = prepared.extracted {
        let stored = extracted.iter().try_for_each(|term| {
            store
                .insert_with_evidence(term, &extraction.source_text, &extraction.target_text)
                .map(|_| ())
        });
        if stored.is_ok()
            && store
                .complete_extraction(&extraction.chapter_id, &extraction.batch_key)
                .is_ok()
        {
            clear_pending_extraction(&mut chapters[chapter_index], &extraction.batch_key);
        }
    }
    state::write_chapter(state_dir, project, &chapters[chapter_index])?;
    // 初稿已变化，旧润色快照必须作废。
    polish::invalidate_round(state_dir, &project.id)?;
    state::append_log(
        state_dir,
        project,
        "retranslated",
        serde_json::json!({ "item_id": prepared.item_id }),
    )
}

/// 记录重译失败信息，界面据此展示错误。
pub(crate) fn mark_retranslation_error(
    state_dir: &Path,
    project: &ProjectState,
    chapters: &mut [Chapter],
    item_id: &str,
    error: &str,
) -> Result<(), String> {
    if let Some(chapter_id) = item_id.strip_prefix("title:") {
        if let Some(chapter) = chapters.iter_mut().find(|chapter| chapter.id == chapter_id) {
            if !chapter.meta.is_object() {
                chapter.meta = serde_json::json!({});
            }
            chapter.meta["retranslation_error"] = serde_json::Value::String(error.to_string());
            return state::write_chapter(state_dir, project, chapter);
        }
    }
    if let Some(chapter) = chapters
        .iter_mut()
        .find(|chapter| chapter.segments.iter().any(|segment| segment.id == item_id))
    {
        if let Some(segment) = chapter
            .segments
            .iter_mut()
            .find(|segment| segment.id == item_id)
        {
            if !segment.meta.is_object() {
                segment.meta = serde_json::json!({});
            }
            segment.meta["retranslation_error"] = serde_json::Value::String(error.to_string());
        }
        return state::write_chapter(state_dir, project, chapter);
    }
    Ok(())
}
