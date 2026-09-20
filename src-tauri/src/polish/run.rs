//! 润色轮次执行：固定快照、有限并发、由单一协调者校验并按序写回批次结果。

use super::plan::{drafts_digest, new_round_id, plan_batches, terms_digest};
use super::store::{invalidate_round, round_path};
use super::{
    book_translation_complete, now, truncate, PolishBatch, PolishBatchStatus, PolishRound,
    PolishSummary,
};
use crate::analysis::BookAnalysis;
use crate::config::AppConfig;
use crate::llm::{RecentTarget, TranslationClient};
use crate::model::{Chapter, PolishStatus, ProjectState, Segment};
use crate::pipeline;
use crate::state;
use crate::terms::{self, Term};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tokio::task::JoinSet;

const MAX_ERROR_CHARS: usize = 500;

type BatchJoin = (usize, Result<Vec<String>, String>, u64);

/// 运行（或恢复）一轮润色。
///
/// 开始前校验全书翻译已完成；旧轮次仍匹配当前草稿与术语时继续使用，
/// 否则记录日志并重新规划批次。
#[allow(clippy::too_many_arguments)]
pub async fn run_round(
    client: Arc<dyn TranslationClient>,
    state_dir: &Path,
    project: &ProjectState,
    chapters: &mut [Chapter],
    analysis: &BookAnalysis,
    terms: &[Term],
    config: &AppConfig,
    retry_failed: bool,
) -> Result<PolishSummary, String> {
    if !book_translation_complete(chapters) {
        return Err("全书翻译尚未完成，暂不能开始润色".to_string());
    }
    let path = round_path(state_dir, &project.id);
    let input_digest = drafts_digest(chapters);
    let terms_digest = terms_digest(terms);

    let existing = if path.is_file() {
        let round: PolishRound = state::read_json(&path)?;
        if round.input_digest == input_digest && round.terms_digest == terms_digest {
            Some(round)
        } else {
            state::append_log(
                state_dir,
                project,
                "polish_invalidated",
                serde_json::json!({ "round_id": round.id, "reason": "input_changed" }),
            )?;
            invalidate_round(state_dir, &project.id)?;
            None
        }
    } else {
        None
    };

    let mut round = match existing {
        Some(mut round) => {
            // 上次运行中断留下的 Running 批次重新排队。
            for batch in round.batches.iter_mut() {
                if batch.status == PolishBatchStatus::Running {
                    batch.status = PolishBatchStatus::Pending;
                }
            }
            if retry_failed {
                for batch in round.batches.iter_mut() {
                    if batch.status == PolishBatchStatus::Failed {
                        batch.status = PolishBatchStatus::Pending;
                        batch.error = None;
                    }
                }
                round.last_error = None;
            }
            if round.finished && !retry_failed {
                return Ok(round.summary());
            }
            round.finished = false;
            round
        }
        None => {
            let round_id = new_round_id();
            let planned = plan_batches(&round_id, chapters, config.segment.max_chars_per_batch);
            if planned.is_empty() {
                return Err("没有需要润色的初稿段落".to_string());
            }
            for chapter in chapters.iter_mut() {
                let chapter_ids = planned
                    .iter()
                    .filter(|batch| batch.chapter_id == chapter.id)
                    .flat_map(|batch| batch.segment_ids.iter())
                    .collect::<Vec<_>>();
                if chapter_ids.is_empty() {
                    continue;
                }
                for segment in chapter.segments.iter_mut() {
                    if chapter_ids.contains(&&segment.id) {
                        segment.polish_status = Some(PolishStatus::Pending);
                    }
                }
                state::write_chapter(state_dir, project, chapter)?;
            }
            let chapter_digests = chapters
                .iter()
                .map(|chapter| {
                    (
                        chapter.id.clone(),
                        chapter
                            .meta
                            .get("source_digest")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                    )
                })
                .collect();
            PolishRound {
                id: round_id,
                created_at: now(),
                updated_at: now(),
                input_digest,
                terms_digest,
                terms: terms.to_vec(),
                style_guide: analysis.style_guide.clone(),
                book_synopsis: analysis.book_synopsis.clone(),
                chapter_digests,
                batches: planned
                    .into_iter()
                    .map(|batch| PolishBatch {
                        id: batch.id,
                        chapter_id: batch.chapter_id,
                        segment_ids: batch.segment_ids,
                        status: PolishBatchStatus::Pending,
                        attempts: 0,
                        error: None,
                        elapsed_ms: None,
                    })
                    .collect(),
                finished: false,
                last_error: None,
            }
        }
    };

    let concurrency = config.general.polish_concurrency.max(1);
    state::write_json_atomic(&path, &round)?;
    state::append_log(
        state_dir,
        project,
        "polish_started",
        serde_json::json!({
            "round_id": round.id,
            "batches": round.batches.len(),
            "concurrency": concurrency,
            "retry_failed": retry_failed,
        }),
    )?;

    let round_terms = Arc::new(round.terms.clone());
    let round_style = Arc::new(round.style_guide.clone());
    let round_synopsis = Arc::new(round.book_synopsis.clone());
    let mut pending = round
        .batches
        .iter()
        .enumerate()
        .filter(|(_, batch)| batch.status == PolishBatchStatus::Pending)
        .map(|(index, _)| index)
        .collect::<VecDeque<_>>();
    let mut tasks: JoinSet<BatchJoin> = JoinSet::new();

    while !pending.is_empty() || !tasks.is_empty() {
        while tasks.len() < concurrency {
            let Some(index) = pending.pop_front() else {
                break;
            };
            let batch = &round.batches[index];
            let chapter_id = batch.chapter_id.clone();
            let segment_ids = batch.segment_ids.clone();
            let chapter_index = chapters
                .iter()
                .position(|chapter| chapter.id == chapter_id)
                .ok_or_else(|| format!("polish batch references missing chapter {chapter_id}"))?;
            let segments = collect_segments(chapters, chapter_index, &segment_ids)?;
            let first_index = chapters[chapter_index]
                .segments
                .iter()
                .position(|segment| segment.id == segment_ids[0])
                .unwrap_or_default();
            // 参考上下文固定读取润色前草稿，避免并行批次互相影响。
            let reference = pipeline::recent_drafts(
                chapters,
                chapter_index,
                first_index,
                config.pipeline.recent_context_chars,
            );
            let digest = round.chapter_digests.get(&chapter_id).cloned().flatten();
            round.batches[index].status = PolishBatchStatus::Running;
            round.batches[index].attempts += 1;
            round.updated_at = now();
            state::write_json_atomic(&path, &round)?;
            spawn_batch(
                &mut tasks,
                &client,
                Arc::clone(&round_terms),
                Arc::clone(&round_style),
                Arc::clone(&round_synopsis),
                digest,
                segments,
                reference,
                config.llm.max_retries,
                index,
            );
        }

        let Some(joined) = tasks.join_next().await else {
            break;
        };
        let (index, result, elapsed_ms) =
            joined.map_err(|error| format!("polish worker failed: {error}"))?;
        let batch_id = round.batches[index].id.clone();
        match result {
            Ok(translations) => {
                let segment_ids = round.batches[index].segment_ids.clone();
                if translations.len() != segment_ids.len() {
                    fail_batch(
                        &mut round.batches[index],
                        format!(
                            "polish returned {} translations for {} segments",
                            translations.len(),
                            segment_ids.len()
                        ),
                        elapsed_ms,
                    );
                    mark_batch_failed(
                        state_dir,
                        project,
                        chapters,
                        &round.batches[index].chapter_id,
                        &segment_ids,
                    )?;
                } else {
                    apply_batch(
                        state_dir,
                        project,
                        chapters,
                        &round.batches[index].chapter_id,
                        &segment_ids,
                        &translations,
                        client.model_name(),
                    )?;
                    round.batches[index].status = PolishBatchStatus::Succeeded;
                    round.batches[index].elapsed_ms = Some(elapsed_ms);
                    state::append_log(
                        state_dir,
                        project,
                        "polish_batch_completed",
                        serde_json::json!({
                            "round_id": round.id,
                            "batch_id": batch_id,
                            "elapsed_ms": elapsed_ms,
                            "attempts": round.batches[index].attempts,
                        }),
                    )?;
                }
            }
            Err(error) => {
                let segment_ids = round.batches[index].segment_ids.clone();
                fail_batch(&mut round.batches[index], error.clone(), elapsed_ms);
                mark_batch_failed(
                    state_dir,
                    project,
                    chapters,
                    &round.batches[index].chapter_id,
                    &segment_ids,
                )?;
                state::append_log(
                    state_dir,
                    project,
                    "polish_batch_failed",
                    serde_json::json!({
                        "round_id": round.id,
                        "batch_id": batch_id,
                        "elapsed_ms": elapsed_ms,
                        "attempts": round.batches[index].attempts,
                        "error": round.batches[index].error,
                    }),
                )?;
            }
        }
        round.updated_at = now();
        state::write_json_atomic(&path, &round)?;
    }

    round.finished = true;
    round.updated_at = now();
    let failed = round
        .batches
        .iter()
        .filter(|batch| batch.status == PolishBatchStatus::Failed)
        .count();
    round.last_error = if failed > 0 {
        Some(format!("{failed} 个批次润色失败，可重试失败批次"))
    } else {
        None
    };
    state::write_json_atomic(&path, &round)?;
    let summary = round.summary();
    state::append_log(
        state_dir,
        project,
        "polish_completed",
        serde_json::json!({
            "round_id": round.id,
            "total": summary.total,
            "succeeded": summary.succeeded,
            "failed": summary.failed,
        }),
    )?;
    Ok(summary)
}

fn collect_segments(
    chapters: &[Chapter],
    chapter_index: usize,
    segment_ids: &[String],
) -> Result<Vec<Segment>, String> {
    segment_ids
        .iter()
        .map(|id| {
            chapters[chapter_index]
                .segments
                .iter()
                .find(|segment| &segment.id == id)
                .cloned()
                .ok_or_else(|| format!("polish batch references missing segment {id}"))
        })
        .collect()
}

fn fail_batch(batch: &mut PolishBatch, error: String, elapsed_ms: u64) {
    batch.status = PolishBatchStatus::Failed;
    batch.elapsed_ms = Some(elapsed_ms);
    batch.error = Some(truncate(&error, MAX_ERROR_CHARS));
}

/// 在任务池中发起一个批次请求；只有命中文本的术语会进入提示词。
#[allow(clippy::too_many_arguments)]
fn spawn_batch(
    tasks: &mut JoinSet<BatchJoin>,
    client: &Arc<dyn TranslationClient>,
    round_terms: Arc<Vec<Term>>,
    round_style: Arc<Vec<String>>,
    round_synopsis: Arc<Option<String>>,
    digest: Option<String>,
    segments: Vec<Segment>,
    reference: Vec<RecentTarget>,
    max_retries: usize,
    batch_index: usize,
) {
    let client = Arc::clone(client);
    tasks.spawn(async move {
        let joined_sources = segments
            .iter()
            .map(|segment| segment.source.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let filtered = terms::relevant_terms(&round_terms, &joined_sources);
        let context = crate::llm::TranslationContext {
            style_guide: &round_style,
            book_synopsis: round_synopsis.as_deref(),
            chapter_digest: digest.as_deref(),
            terms: &filtered,
            recent_targets: &reference,
            surrounding_source: None,
        };
        let started = Instant::now();
        let result =
            crate::llm::polish_batch(client.as_ref(), &segments, &context, max_retries).await;
        let elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        (batch_index, result, elapsed_ms)
    });
}

fn apply_batch(
    state_dir: &Path,
    project: &ProjectState,
    chapters: &mut [Chapter],
    chapter_id: &str,
    segment_ids: &[String],
    translations: &[String],
    model: Option<&str>,
) -> Result<(), String> {
    let chapter_index = chapters
        .iter()
        .position(|chapter| chapter.id == chapter_id)
        .ok_or_else(|| format!("polish batch references missing chapter {chapter_id}"))?;
    for (id, translation) in segment_ids.iter().zip(translations) {
        let segment = chapters[chapter_index]
            .segments
            .iter_mut()
            .find(|segment| &segment.id == id)
            .ok_or_else(|| format!("polish batch references missing segment {id}"))?;
        if crate::revisions::is_protected(segment) {
            continue;
        }
        crate::revisions::set_target(
            segment,
            translation.clone(),
            crate::revisions::RevisionKind::Polish,
            model,
        )?;
        segment.polish_status = Some(PolishStatus::Succeeded);
    }
    state::write_chapter(state_dir, project, &chapters[chapter_index])
}

/// 标记批次内未成功的段落为失败，已成功的段落保持不变。
fn mark_batch_failed(
    state_dir: &Path,
    project: &ProjectState,
    chapters: &mut [Chapter],
    chapter_id: &str,
    segment_ids: &[String],
) -> Result<(), String> {
    let chapter_index = chapters
        .iter()
        .position(|chapter| chapter.id == chapter_id)
        .ok_or_else(|| format!("polish batch references missing chapter {chapter_id}"))?;
    let mut changed = false;
    for id in segment_ids {
        if let Some(segment) = chapters[chapter_index]
            .segments
            .iter_mut()
            .find(|segment| &segment.id == id)
        {
            if segment.polish_status != Some(PolishStatus::Succeeded) {
                segment.polish_status = Some(PolishStatus::Failed);
                changed = true;
            }
        }
    }
    if changed {
        state::write_chapter(state_dir, project, &chapters[chapter_index])?;
    }
    Ok(())
}
