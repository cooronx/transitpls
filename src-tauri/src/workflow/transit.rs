//! 主翻译流程：按章节和字符预算分批翻译，失败时回退到逐段翻译。

use super::common::{
    build_client, chapter_body_complete, load_translation_analysis, save_project_progress,
    term_store,
};
use super::extraction::{
    clear_pending_extraction, extract_completed_chapter, process_extraction,
    record_pending_extraction, retry_pending_extractions,
};
use super::titles::translate_missing_titles;
use crate::analysis::BookAnalysis;
use crate::config::{self, AppConfig};
use crate::llm::{self, RecentTarget, TranslationClient};
use crate::model::{Chapter, ItemStatus, PolishStatus, ProjectState, ProjectStatus, Segment};
use crate::pipeline;
use crate::polish;
use crate::state;
use crate::terms::TermStore;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 界面入口：加载配置后运行翻译。
pub async fn transit_project(
    config_path: Option<PathBuf>,
    input: PathBuf,
    chapter: Option<usize>,
    mock_client: bool,
) -> Result<ProjectState, String> {
    let loaded = config::load(config_path.as_deref())?;
    transit(
        input,
        chapter,
        mock_client,
        &loaded.state_dir,
        &loaded.value,
    )
    .await
}

/// 运行翻译并在完成后按配置决定是否自动润色。
pub(crate) async fn transit(
    input: PathBuf,
    chapter: Option<usize>,
    mock: bool,
    state_dir: &Path,
    config: &AppConfig,
) -> Result<ProjectState, String> {
    let mut project = state::load_for_source(state_dir, &input)?;
    let _lock = state::acquire_project_lock(state_dir, &project)?;
    let mut chapters = state::load_chapters(state_dir, &project)?;
    if let Some(chapter) = chapter {
        if chapter >= chapters.len() {
            return Err(format!(
                "chapter index {chapter} is out of range; project has {} chapters",
                chapters.len()
            ));
        }
    }
    let analysis = load_translation_analysis(state_dir, &project, &chapters, config)?;
    let client: Arc<dyn TranslationClient> =
        Arc::from(build_client(config, mock, state_dir, &project)?);
    let store = term_store(state_dir, &project)?;
    state::append_log(
        state_dir,
        &project,
        "transit_started",
        serde_json::json!({ "mock": mock, "chapter": chapter }),
    )?;
    project.status = ProjectStatus::Translating;
    state::save_project(state_dir, &project)?;

    let result = run_transit(
        client.as_ref(),
        &store,
        state_dir,
        &mut project,
        &mut chapters,
        &analysis,
        config,
        chapter,
    )
    .await;
    if let Err(error) = result {
        state::mark_failed(state_dir, &mut project, &error)?;
        return Err(error);
    }
    state::append_log(
        state_dir,
        &project,
        "transit_completed",
        serde_json::json!({ "chapters": project.chapters_completed }),
    )?;
    if chapter.is_none() && config.pipeline.polish && polish::book_translation_complete(&chapters) {
        let terms = store.list()?;
        if let Err(error) = polish::run_round(
            Arc::clone(&client),
            state_dir,
            &project,
            &mut chapters,
            &analysis,
            &terms,
            config,
            false,
        )
        .await
        {
            // 译文已保存，润色失败不应把项目标记为失败。
            state::append_log(
                state_dir,
                &project,
                "polish_failed",
                serde_json::json!({ "error": error }),
            )?;
            return Err(error);
        }
        save_project_progress(state_dir, &mut project, &chapters)?;
    }
    Ok(project)
}

/// 翻译指定章节（或全书），最后补齐章节标题。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_transit<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    state_dir: &Path,
    project: &mut ProjectState,
    chapters: &mut [Chapter],
    analysis: &BookAnalysis,
    config: &AppConfig,
    selected_chapter: Option<usize>,
) -> Result<(), String> {
    retry_pending_extractions(
        client,
        store,
        state_dir,
        project,
        chapters,
        config.llm.max_retries,
        config.pipeline.recent_context_chars,
    )
    .await?;

    let chapter_indices = selected_chapter
        .map(|index| vec![index])
        .unwrap_or_else(|| (0..chapters.len()).collect());
    for chapter_index in chapter_indices {
        translate_chapter(
            client,
            store,
            state_dir,
            project,
            chapters,
            chapter_index,
            analysis,
            config,
        )
        .await?;
    }

    if chapters.iter().all(chapter_body_complete) {
        translate_missing_titles(
            client, store, state_dir, project, chapters, analysis, config,
        )
        .await?;
    }
    save_project_progress(state_dir, project, chapters)
}

/// 翻译单章：按批次范围翻译未完成段落，整批失败时逐段重试。
#[allow(clippy::too_many_arguments)]
async fn translate_chapter<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    state_dir: &Path,
    project: &mut ProjectState,
    chapters: &mut [Chapter],
    chapter_index: usize,
    analysis: &BookAnalysis,
    config: &AppConfig,
) -> Result<(), String> {
    let digest = chapters[chapter_index]
        .meta
        .get("source_digest")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let ranges = pipeline::batch_ranges(
        &chapters[chapter_index].segments,
        config.segment.max_chars_per_batch,
    );

    for range in ranges {
        let untranslated = range
            .clone()
            .filter(|&index| {
                let segment = &chapters[chapter_index].segments[index];
                segment.target.is_none() && segment.target_before_polish.is_none()
            })
            .collect::<Vec<_>>();
        if !untranslated.is_empty() {
            let request = cloned_segments(chapters, chapter_index, &untranslated);
            let relevant_terms = store.relevant(
                &request
                    .iter()
                    .map(|segment| segment.source.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            )?;
            let recent = pipeline::recent_targets(
                chapters,
                chapter_index,
                untranslated[0],
                config.pipeline.recent_context_chars,
            );
            match request_translation(
                client,
                &request,
                project,
                analysis,
                digest.as_deref(),
                &relevant_terms,
                &recent,
                config.llm.max_retries,
            )
            .await
            {
                Ok(translations) => {
                    for (&index, translation) in untranslated.iter().zip(translations) {
                        store_draft(&mut chapters[chapter_index].segments[index], translation);
                    }
                    state::write_chapter(state_dir, project, &chapters[chapter_index])?;
                    finalize_segments(
                        client,
                        store,
                        state_dir,
                        project,
                        chapters,
                        chapter_index,
                        &untranslated,
                        config,
                    )
                    .await?;
                }
                Err(_) => {
                    for &index in &untranslated {
                        translate_one_with_fallback(
                            client,
                            store,
                            state_dir,
                            project,
                            chapters,
                            chapter_index,
                            index,
                            analysis,
                            digest.as_deref(),
                            config,
                        )
                        .await?;
                    }
                }
            }
        }

        pipeline::write_context(
            &state::project_dir(state_dir, &project.id),
            chapters,
            chapter_index,
            range.end,
            config.pipeline.recent_context_chars,
        )?;
    }

    extract_completed_chapter(
        client,
        store,
        state_dir,
        project,
        chapters,
        chapter_index,
        config,
    )
    .await
}

fn cloned_segments(chapters: &[Chapter], chapter_index: usize, indices: &[usize]) -> Vec<Segment> {
    indices
        .iter()
        .map(|&index| chapters[chapter_index].segments[index].clone())
        .collect()
}

/// 保存初稿：`target_before_polish` 同时作为润色输入和可导出的译文，
/// `polish_status` 记录该草稿是否已经润色过。
fn store_draft(segment: &mut Segment, value: String) {
    segment.target_before_polish = Some(value.clone());
    segment.target = Some(value);
    segment.polish_status = Some(PolishStatus::Pending);
    segment.status = ItemStatus::Translated;
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn request_translation<C: TranslationClient + ?Sized>(
    client: &C,
    segments: &[Segment],
    project: &ProjectState,
    analysis: &BookAnalysis,
    digest: Option<&str>,
    terms: &[crate::terms::Term],
    recent: &[RecentTarget],
    max_retries: usize,
) -> Result<Vec<String>, String> {
    llm::translate_batch(
        client,
        segments,
        &project.source_language,
        &project.target_language,
        &llm::TranslationContext {
            style_guide: &analysis.style_guide,
            book_synopsis: analysis.book_synopsis.as_deref(),
            chapter_digest: digest,
            terms,
            recent_targets: recent,
        },
        max_retries,
    )
    .await
}

/// 单段翻译兜底：失败时把该段标记为 `Failed`，成功则走与批次相同的收尾流程。
#[allow(clippy::too_many_arguments)]
async fn translate_one_with_fallback<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    state_dir: &Path,
    project: &mut ProjectState,
    chapters: &mut [Chapter],
    chapter_index: usize,
    segment_index: usize,
    analysis: &BookAnalysis,
    digest: Option<&str>,
    config: &AppConfig,
) -> Result<(), String> {
    let recent = pipeline::recent_targets(
        chapters,
        chapter_index,
        segment_index,
        config.pipeline.recent_context_chars,
    );
    let segment = chapters[chapter_index].segments[segment_index].clone();
    let terms = store.relevant(&segment.source)?;
    let translations = request_translation(
        client,
        std::slice::from_ref(&segment),
        project,
        analysis,
        digest,
        &terms,
        &recent,
        config.llm.max_retries,
    )
    .await;
    let translation = match translations {
        Ok(mut values) => values.remove(0),
        Err(error) => {
            chapters[chapter_index].segments[segment_index].status = ItemStatus::Failed;
            state::write_chapter(state_dir, project, &chapters[chapter_index])?;
            return Err(format!("segment {} failed: {error}", segment.id));
        }
    };
    store_draft(
        &mut chapters[chapter_index].segments[segment_index],
        translation,
    );
    state::write_chapter(state_dir, project, &chapters[chapter_index])?;
    finalize_segments(
        client,
        store,
        state_dir,
        project,
        chapters,
        chapter_index,
        &[segment_index],
        config,
    )
    .await
}

/// 批次收尾：更新状态、登记待抽取、写回章节并立即尝试术语抽取。
#[allow(clippy::too_many_arguments)]
async fn finalize_segments<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    state_dir: &Path,
    project: &mut ProjectState,
    chapters: &mut [Chapter],
    chapter_index: usize,
    indices: &[usize],
    config: &AppConfig,
) -> Result<(), String> {
    if indices.is_empty() {
        return Ok(());
    }
    for &index in indices {
        chapters[chapter_index].segments[index].status = ItemStatus::Translated;
    }
    chapters[chapter_index].status = if chapter_body_complete(&chapters[chapter_index]) {
        ItemStatus::Translated
    } else {
        ItemStatus::Pending
    };
    let extraction = crate::terms::PendingExtraction {
        chapter_id: chapters[chapter_index].id.clone(),
        batch_key: indices
            .iter()
            .map(|&index| chapters[chapter_index].segments[index].id.as_str())
            .collect::<Vec<_>>()
            .join("|"),
        source_text: indices
            .iter()
            .map(|&index| chapters[chapter_index].segments[index].source.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        target_text: indices
            .iter()
            .filter_map(|&index| chapters[chapter_index].segments[index].target.as_deref())
            .collect::<Vec<_>>()
            .join("\n"),
    };
    record_pending_extraction(&mut chapters[chapter_index], &extraction)?;
    state::write_chapter(state_dir, project, &chapters[chapter_index])?;
    pipeline::write_context(
        &state::project_dir(state_dir, &project.id),
        chapters,
        chapter_index,
        indices.iter().copied().max().unwrap_or_default() + 1,
        config.pipeline.recent_context_chars,
    )?;
    save_project_progress(state_dir, project, chapters)?;
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
        // Term extraction is auxiliary: keep translation progress and retry
        // the pending extraction on a later run.
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
        return Ok(());
    }
    clear_pending_extraction(&mut chapters[chapter_index], &extraction.batch_key);
    state::write_chapter(state_dir, project, &chapters[chapter_index])
}
