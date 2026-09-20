//! 主翻译流程：按章节和字符预算分批翻译，失败时回退到逐段翻译。
//!
//! `general.translation_concurrency` 大于 1 时由批次调度器并发执行：
//! worker 只读快照调用模型，协调者在单线程中按批次位置写回状态。

use super::common::{
    build_client, chapter_body_complete, load_translation_analysis, save_project_progress,
    term_store,
};
use super::extraction::{
    clear_pending_extraction, extract_completed_chapter, process_extraction,
    record_pending_extraction, retry_pending_extractions, store_extraction,
};
use super::titles::translate_missing_titles;
use crate::analysis::BookAnalysis;
use crate::config::{self, AppConfig};
use crate::llm::{self, RecentTarget, TranslationClient};
use crate::model::{Chapter, ItemStatus, PolishStatus, ProjectState, ProjectStatus, Segment};
use crate::pipeline;
use crate::polish;
use crate::state;
use crate::terms::{Term, TermStore};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::task::JoinSet;

/// 连续失败熔断阈值：达到后中止本轮翻译，避免服务不可用时继续发请求。
const MAX_CONSECUTIVE_FAILURES: usize = 5;

/// 本轮翻译的连续失败计数；任一成功即清零。
#[derive(Default)]
struct FailureTracker {
    consecutive: usize,
}

impl FailureTracker {
    fn record_success(&mut self) {
        self.consecutive = 0;
    }

    /// 记录一次最终失败；达到阈值时返回 `true`。
    fn record_failure(&mut self) -> bool {
        self.consecutive += 1;
        self.consecutive >= MAX_CONSECUTIVE_FAILURES
    }
}

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
        Arc::clone(&client),
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
    if chapter.is_none()
        && config.pipeline.polish
        && polish::book_translation_complete(&chapters)
        && polish::pending_segment_count(&chapters) > 0
    {
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
pub(crate) async fn run_transit(
    client: Arc<dyn TranslationClient>,
    store: &TermStore,
    state_dir: &Path,
    project: &mut ProjectState,
    chapters: &mut [Chapter],
    analysis: &BookAnalysis,
    config: &AppConfig,
    selected_chapter: Option<usize>,
) -> Result<(), String> {
    retry_pending_extractions(
        client.as_ref(),
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
    let mut failures = FailureTracker::default();
    if config.general.translation_concurrency <= 1 {
        // 串行模式保留批次间最近译文参考。
        for chapter_index in chapter_indices {
            translate_chapter(
                client.as_ref(),
                store,
                state_dir,
                project,
                chapters,
                chapter_index,
                analysis,
                config,
                &mut failures,
            )
            .await?;
        }
    } else {
        translate_batches_concurrent(
            &client,
            store,
            state_dir,
            project,
            chapters,
            analysis,
            config,
            &chapter_indices,
            &mut failures,
        )
        .await?;
    }

    if chapters.iter().all(chapter_body_complete) {
        translate_missing_titles(
            client.as_ref(),
            store,
            state_dir,
            project,
            chapters,
            analysis,
            config,
        )
        .await?;
    }

    let failed_segments = chapters
        .iter()
        .flat_map(|chapter| chapter.segments.iter())
        .filter(|segment| segment.status == ItemStatus::Failed)
        .collect::<Vec<_>>();
    if !failed_segments.is_empty() {
        state::append_log(
            state_dir,
            project,
            "translation_failed_segments",
            serde_json::json!({
                "count": failed_segments.len(),
                "segments": failed_segments
                    .iter()
                    .take(20)
                    .map(|segment| segment.id.as_str())
                    .collect::<Vec<_>>(),
            }),
        )?;
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
    failures: &mut FailureTracker,
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
            let surrounding = pipeline::surrounding_source(
                &chapters[chapter_index].segments,
                untranslated[0]..untranslated[untranslated.len() - 1] + 1,
            );
            match request_translation(
                client,
                &request,
                project,
                analysis,
                digest.as_deref(),
                &relevant_terms,
                &recent,
                &surrounding,
                config.llm.max_retries,
            )
            .await
            {
                Ok(translations) => {
                    for (&index, translation) in untranslated.iter().zip(translations) {
                        store_draft(
                            &mut chapters[chapter_index].segments[index],
                            translation,
                            client.model_name(),
                        )?;
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
                        None,
                    )
                    .await?;
                    failures.record_success();
                }
                Err(_) => {
                    for &index in &untranslated {
                        let recent = pipeline::recent_targets(
                            chapters,
                            chapter_index,
                            index,
                            config.pipeline.recent_context_chars,
                        );
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
                            &recent,
                            config,
                            failures,
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

/// 一个待翻译批次的调度快照：批次在原书中的位置、原文片段与请求固定上下文。
#[derive(Clone)]
struct BatchPlan {
    chapter_index: usize,
    indices: Vec<usize>,
    digest: Option<String>,
    segments: Vec<Segment>,
    surrounding_source: llm::SurroundingSource,
}

/// worker 返回给协调者的结果；worker 自身不写任何项目状态。
struct PreparedBatch {
    translations: Result<Vec<String>, String>,
    extracted: Option<Result<Vec<Term>, String>>,
}

type BatchJoin = (usize, PreparedBatch);

/// 把待翻译的批次铺成队列；已翻译的批次在开始前跳过。
fn plan_batches(
    chapters: &[Chapter],
    chapter_indices: &[usize],
    max_chars: usize,
) -> Vec<BatchPlan> {
    let mut plans = Vec::new();
    for &chapter_index in chapter_indices {
        let chapter = &chapters[chapter_index];
        let digest = chapter
            .meta
            .get("source_digest")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        for range in pipeline::batch_ranges(&chapter.segments, max_chars) {
            let indices = range
                .filter(|&index| {
                    let segment = &chapter.segments[index];
                    segment.target.is_none() && segment.target_before_polish.is_none()
                })
                .collect::<Vec<_>>();
            if indices.is_empty() {
                continue;
            }
            plans.push(BatchPlan {
                chapter_index,
                surrounding_source: pipeline::surrounding_source(
                    &chapter.segments,
                    indices[0]..indices[indices.len() - 1] + 1,
                ),
                segments: indices
                    .iter()
                    .map(|&index| chapter.segments[index].clone())
                    .collect(),
                indices,
                digest: digest.clone(),
            });
        }
    }
    plans
}

/// 并发翻译：最多 N 个批次同时在飞，完成一批就按其在原书中的位置写回一批。
#[allow(clippy::too_many_arguments)]
async fn translate_batches_concurrent(
    client: &Arc<dyn TranslationClient>,
    store: &TermStore,
    state_dir: &Path,
    project: &mut ProjectState,
    chapters: &mut [Chapter],
    analysis: &BookAnalysis,
    config: &AppConfig,
    chapter_indices: &[usize],
    failures: &mut FailureTracker,
) -> Result<(), String> {
    let concurrency = config.general.translation_concurrency.max(1);
    let plans = plan_batches(
        chapters,
        chapter_indices,
        config.segment.max_chars_per_batch,
    );
    let project_snapshot = Arc::new(project.clone());
    let analysis_snapshot = Arc::new(analysis.clone());
    let mut pending = (0..plans.len()).collect::<VecDeque<_>>();
    let mut tasks: JoinSet<BatchJoin> = JoinSet::new();

    while !pending.is_empty() || !tasks.is_empty() {
        while tasks.len() < concurrency {
            let Some(plan_index) = pending.pop_front() else {
                break;
            };
            spawn_translation_batch(
                &mut tasks,
                client,
                store,
                &project_snapshot,
                &analysis_snapshot,
                config.llm.max_retries,
                plan_index,
                plans[plan_index].clone(),
            );
        }
        // 完成顺序与批次顺序无关，写回只依据批次位置；每个结果在下一个
        // await 之前落盘，中断或取消最多丢失在途批次，不丢已完成进度。
        let Some(joined) = tasks.join_next().await else {
            break;
        };
        let (plan_index, prepared) =
            joined.map_err(|error| format!("translation worker failed: {error}"))?;
        apply_prepared_batch(
            client,
            store,
            state_dir,
            project,
            chapters,
            analysis,
            config,
            &plans[plan_index],
            prepared,
            failures,
        )
        .await?;
    }
    Ok(())
}

/// 在任务池中发起一个批次请求；worker 只读快照并调用模型。
#[allow(clippy::too_many_arguments)]
fn spawn_translation_batch(
    tasks: &mut JoinSet<BatchJoin>,
    client: &Arc<dyn TranslationClient>,
    store: &TermStore,
    project: &Arc<ProjectState>,
    analysis: &Arc<BookAnalysis>,
    max_retries: usize,
    plan_index: usize,
    plan: BatchPlan,
) {
    let client = Arc::clone(client);
    let store = store.clone();
    let project = Arc::clone(project);
    let analysis = Arc::clone(analysis);
    tasks.spawn(async move {
        let joined_sources = plan
            .segments
            .iter()
            .map(|segment| segment.source.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let translations = match store.relevant(&joined_sources) {
            Ok(terms) => {
                // 并发模式下不再注入最近译文，避免依赖完成顺序。
                request_translation(
                    client.as_ref(),
                    &plan.segments,
                    &project,
                    &analysis,
                    plan.digest.as_deref(),
                    &terms,
                    &[],
                    &plan.surrounding_source,
                    max_retries,
                )
                .await
            }
            Err(error) => Err(error),
        };
        let known_terms = store.relevant(&joined_sources).unwrap_or_default();
        let extracted = match &translations {
            Ok(translations) => {
                let target_text = translations
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join("\n");
                Some(
                    crate::terms::extract_terms_resilient(
                        client.as_ref(),
                        &joined_sources,
                        &target_text,
                        &known_terms,
                        plan.chapter_index,
                        max_retries,
                    )
                    .await,
                )
            }
            Err(_) => None,
        };
        (
            plan_index,
            PreparedBatch {
                translations,
                extracted,
            },
        )
    });
}

/// 协调者写回：成功批次按位置写入草稿并收尾，失败批次回退到逐段翻译。
#[allow(clippy::too_many_arguments)]
async fn apply_prepared_batch(
    client: &Arc<dyn TranslationClient>,
    store: &TermStore,
    state_dir: &Path,
    project: &mut ProjectState,
    chapters: &mut [Chapter],
    analysis: &BookAnalysis,
    config: &AppConfig,
    plan: &BatchPlan,
    prepared: PreparedBatch,
    failures: &mut FailureTracker,
) -> Result<(), String> {
    let chapter_index = plan.chapter_index;
    match prepared.translations {
        Ok(translations) => {
            for (&index, translation) in plan.indices.iter().zip(translations) {
                store_draft(
                    &mut chapters[chapter_index].segments[index],
                    translation,
                    client.model_name(),
                )?;
            }
            state::write_chapter(state_dir, project, &chapters[chapter_index])?;
            finalize_segments(
                client.as_ref(),
                store,
                state_dir,
                project,
                chapters,
                chapter_index,
                &plan.indices,
                config,
                prepared.extracted,
            )
            .await?;
            failures.record_success();
        }
        Err(error) => {
            state::append_log(
                state_dir,
                project,
                "translation_batch_failed",
                serde_json::json!({
                    "chapter_id": chapters[chapter_index].id,
                    "segments": plan.indices.len(),
                    "error": error,
                }),
            )?;
            for &index in &plan.indices {
                translate_one_with_fallback(
                    client.as_ref(),
                    store,
                    state_dir,
                    project,
                    chapters,
                    chapter_index,
                    index,
                    analysis,
                    plan.digest.as_deref(),
                    // 并发模式与批次请求一致，不注入最近译文。
                    &[],
                    config,
                    failures,
                )
                .await?;
            }
        }
    }
    // 整章抽取依赖章节已经完整，且包含模型调用，统一放回协调者串行执行。
    if chapter_body_complete(&chapters[chapter_index]) {
        extract_completed_chapter(
            client.as_ref(),
            store,
            state_dir,
            project,
            chapters,
            chapter_index,
            config,
        )
        .await?;
    }
    Ok(())
}

/// 保存初稿：`target_before_polish` 同时作为润色输入和可导出的译文，
/// `polish_status` 记录该草稿是否已经润色过。
fn store_draft(segment: &mut Segment, value: String, model: Option<&str>) -> Result<(), String> {
    crate::revisions::set_target(
        segment,
        value.clone(),
        crate::revisions::RevisionKind::Translation,
        model,
    )?;
    segment.target_before_polish = Some(value);
    segment.polish_status = Some(PolishStatus::Pending);
    segment.status = ItemStatus::Translated;
    Ok(())
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
    surrounding: &llm::SurroundingSource,
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
            surrounding_source: Some(surrounding),
        },
        max_retries,
    )
    .await
}

/// 单段翻译兜底：最终失败时把该段标记为 `Failed` 并跳过，成功则走与批次相同的收尾流程。
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
    recent: &[RecentTarget],
    config: &AppConfig,
    failures: &mut FailureTracker,
) -> Result<(), String> {
    let segment = chapters[chapter_index].segments[segment_index].clone();
    let terms = store.relevant(&segment.source)?;
    let surrounding = pipeline::surrounding_source(
        &chapters[chapter_index].segments,
        segment_index..segment_index + 1,
    );
    let translations = request_translation(
        client,
        std::slice::from_ref(&segment),
        project,
        analysis,
        digest,
        &terms,
        recent,
        &surrounding,
        config.llm.max_retries,
    )
    .await;
    let translation = match translations {
        Ok(mut values) => values.remove(0),
        Err(error) => {
            chapters[chapter_index].segments[segment_index].status = ItemStatus::Failed;
            state::write_chapter(state_dir, project, &chapters[chapter_index])?;
            state::append_log(
                state_dir,
                project,
                "segment_failed",
                serde_json::json!({
                    "chapter_id": chapters[chapter_index].id,
                    "segment_id": segment.id,
                    "error": error,
                }),
            )?;
            if failures.record_failure() {
                return Err(format!(
                    "translation aborted after {MAX_CONSECUTIVE_FAILURES} consecutive failed segments; see the project log"
                ));
            }
            return Ok(());
        }
    };
    store_draft(
        &mut chapters[chapter_index].segments[segment_index],
        translation,
        client.model_name(),
    )?;
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
        None,
    )
    .await?;
    failures.record_success();
    Ok(())
}

/// 批次收尾：更新状态、登记待抽取、写回章节并处理术语抽取。
///
/// `extracted` 为 `Some` 表示术语已在 worker 中抽取完成（并发模式），协调者
/// 只负责入库；为 `None` 时在本函数内调用模型抽取（串行模式）。
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
    extracted: Option<Result<Vec<Term>, String>>,
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
    let outcome = match extracted {
        Some(result) => result.and_then(|terms| store_extraction(store, &extraction, &terms)),
        None => {
            process_extraction(
                client,
                store,
                &extraction,
                chapter_index,
                config.llm.max_retries,
            )
            .await
        }
    };
    if let Err(error) = outcome {
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
