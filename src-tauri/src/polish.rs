use crate::analysis::BookAnalysis;
use crate::config::AppConfig;
use crate::llm::{RecentTarget, TranslationClient};
use crate::model::{Chapter, ItemStatus, PolishStatus, ProjectState, Segment};
use crate::pipeline;
use crate::state;
use crate::terms::{self, Term};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::task::JoinSet;

pub const ROUND_FILE: &str = "polish.json";
const MAX_ERROR_CHARS: usize = 500;

type BatchJoin = (usize, Result<Vec<String>, String>, u64);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PolishBatchStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolishBatch {
    pub id: String,
    pub chapter_id: String,
    pub segment_ids: Vec<String>,
    pub status: PolishBatchStatus,
    #[serde(default)]
    pub attempts: usize,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub elapsed_ms: Option<u64>,
}

/// Fixed input for one polish round. Drafts themselves stay in the chapter files
/// (`target_before_polish`); `input_digest` pins their content so the round is
/// discarded if anything was retranslated after an interruption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolishRound {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub input_digest: String,
    pub terms_digest: String,
    pub terms: Vec<Term>,
    pub style_guide: Vec<String>,
    pub book_synopsis: Option<String>,
    #[serde(default)]
    pub chapter_digests: BTreeMap<String, Option<String>>,
    pub batches: Vec<PolishBatch>,
    #[serde(default)]
    pub finished: bool,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PolishSummary {
    pub round_id: String,
    pub finished: bool,
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub pending: usize,
    #[serde(default)]
    pub pending_segments: usize,
    pub last_error: Option<String>,
    pub updated_at: String,
}

impl PolishRound {
    pub fn summary(&self) -> PolishSummary {
        let succeeded = self
            .batches
            .iter()
            .filter(|batch| batch.status == PolishBatchStatus::Succeeded)
            .count();
        let failed = self
            .batches
            .iter()
            .filter(|batch| batch.status == PolishBatchStatus::Failed)
            .count();
        let pending = self
            .batches
            .iter()
            .filter(|batch| {
                matches!(
                    batch.status,
                    PolishBatchStatus::Pending | PolishBatchStatus::Running
                )
            })
            .count();
        PolishSummary {
            round_id: self.id.clone(),
            finished: self.finished,
            total: self.batches.len(),
            succeeded,
            failed,
            pending,
            pending_segments: 0,
            last_error: self.last_error.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlannedBatch {
    pub id: String,
    pub chapter_id: String,
    pub segment_ids: Vec<String>,
}

pub fn book_translation_complete(chapters: &[Chapter]) -> bool {
    chapters.iter().all(|chapter| {
        chapter.target_title.is_some()
            && chapter
                .segments
                .iter()
                .all(|segment| segment.status == ItemStatus::Translated && segment.target.is_some())
    })
}

fn round_path(state_dir: &Path, project_id: &str) -> PathBuf {
    state::project_dir(state_dir, project_id).join(ROUND_FILE)
}

pub fn pending_segment_count(chapters: &[Chapter]) -> usize {
    chapters
        .iter()
        .flat_map(|chapter| &chapter.segments)
        .filter(|segment| {
            segment.polish_status != Some(PolishStatus::Succeeded)
                && segment
                    .target_before_polish
                    .as_deref()
                    .is_some_and(|draft| !draft.trim().is_empty())
        })
        .count()
}

pub fn read_summary(
    state_dir: &Path,
    project_id: &str,
    chapters: &[Chapter],
) -> Result<Option<PolishSummary>, String> {
    let path = round_path(state_dir, project_id);
    let pending_segments = pending_segment_count(chapters);
    if !path.is_file() {
        return Ok(None);
    }
    let round: PolishRound = state::read_json(&path)?;
    let mut summary = round.summary();
    summary.pending_segments = pending_segments;
    Ok(Some(summary))
}

pub fn invalidate_round(state_dir: &Path, project_id: &str) -> Result<(), String> {
    let path = round_path(state_dir, project_id);
    if !path.exists() {
        return Ok(());
    }
    std::fs::remove_file(&path).map_err(|error| {
        format!(
            "failed to invalidate polish round {}: {error}",
            path.display()
        )
    })
}

pub fn plan_batches(round_id: &str, chapters: &[Chapter], max_chars: usize) -> Vec<PlannedBatch> {
    let mut batches = Vec::new();
    for chapter in chapters {
        let eligible = chapter
            .segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| {
                segment.polish_status != Some(PolishStatus::Succeeded)
                    && segment
                        .target_before_polish
                        .as_deref()
                        .is_some_and(|draft| !draft.trim().is_empty())
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if eligible.is_empty() {
            continue;
        }
        let projected = eligible
            .iter()
            .map(|&index| chapter.segments[index].clone())
            .collect::<Vec<_>>();
        for range in pipeline::batch_ranges(&projected, max_chars) {
            let indices = &eligible[range];
            batches.push(PlannedBatch {
                id: format!("{round_id}-b{:03}", batches.len()),
                chapter_id: chapter.id.clone(),
                segment_ids: indices
                    .iter()
                    .map(|&index| chapter.segments[index].id.clone())
                    .collect(),
            });
        }
    }
    batches
}

/// Runs (or resumes) one polish round: fixed snapshot, bounded parallelism and a
/// single coordinator that validates and applies every batch in ordered fashion.
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
        segment.target = Some(translation.clone());
        segment.polish_status = Some(PolishStatus::Succeeded);
    }
    state::write_chapter(state_dir, project, &chapters[chapter_index])
}

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

fn drafts_digest(chapters: &[Chapter]) -> String {
    let mut hasher = Sha256::new();
    for chapter in chapters {
        for segment in &chapter.segments {
            if let Some(draft) = &segment.target_before_polish {
                hasher.update(segment.id.as_bytes());
                hasher.update([0x1f]);
                hasher.update(draft.as_bytes());
                hasher.update([0x1e]);
            }
        }
    }
    format_digest(hasher.finalize())
}

fn terms_digest(terms: &[Term]) -> String {
    let bytes = serde_json::to_vec(terms).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format_digest(hasher.finalize())
}

fn format_digest(hash: impl AsRef<[u8]>) -> String {
    hash.as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn new_round_id() -> String {
    format!(
        "round-{}",
        Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
    )
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::BookAnalysis;
    use crate::llm::CompletionOutput;
    use crate::model::{Document, DocumentMetadata, SegmentKind};
    use async_trait::async_trait;
    use rig_core::completion::Usage;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    fn temp_dir() -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "transitpls-polish-{}-{nonce}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("temp directory should be created");
        path
    }

    fn document(chapters: usize, segments: usize) -> Document {
        Document {
            metadata: DocumentMetadata {
                title: "Book".to_string(),
                source_language: "en".to_string(),
                target_language: "zh-CN".to_string(),
                source_format: "txt".to_string(),
            },
            chapters: (0..chapters)
                .map(|chapter| Chapter {
                    id: format!("chapter-{chapter}"),
                    title: format!("Chapter {chapter}"),
                    target_title: Some(format!("第 {chapter} 章")),
                    status: ItemStatus::Translated,
                    meta: serde_json::json!({ "source_digest": format!("digest {chapter}") }),
                    segments: (0..segments)
                        .map(|segment| Segment {
                            id: format!("c{chapter}-s{segment}"),
                            ordinal: segment,
                            source: format!("source {chapter}-{segment}"),
                            target: Some(format!("draft {chapter}-{segment}")),
                            target_before_polish: Some(format!("draft {chapter}-{segment}")),
                            polish_status: Some(PolishStatus::Pending),
                            kind: SegmentKind::Paragraph,
                            status: ItemStatus::Translated,
                            source_hash: "hash".to_string(),
                            meta: serde_json::json!({}),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    fn analysis() -> BookAnalysis {
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

    struct Fixture {
        dir: PathBuf,
        state_dir: PathBuf,
        project: ProjectState,
        chapters: Vec<Chapter>,
    }

    fn fixture(chapters: usize, segments: usize) -> Fixture {
        let dir = temp_dir();
        let source = dir.join("book.txt");
        std::fs::write(&source, "book").expect("source should be written");
        let state_dir = dir.join("projects");
        let initialized =
            state::initialize(&state_dir, &source, &document(chapters, segments), 1_200)
                .expect("project should initialize");
        let chapters =
            state::load_chapters(&state_dir, &initialized.project).expect("chapters should load");
        Fixture {
            dir,
            state_dir,
            project: initialized.project,
            chapters,
        }
    }

    struct ControlledClient {
        delay_ms: u64,
        slow_ids: HashSet<String>,
        fail_ids: HashSet<String>,
        active: AtomicUsize,
        max_active: AtomicUsize,
        processed: Mutex<Vec<String>>,
    }

    impl ControlledClient {
        fn new(delay_ms: u64) -> Self {
            Self {
                delay_ms,
                slow_ids: HashSet::new(),
                fail_ids: HashSet::new(),
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
                processed: Mutex::new(Vec::new()),
            }
        }

        fn slow(mut self, id: &str) -> Self {
            self.slow_ids.insert(id.to_string());
            self
        }

        fn fail(mut self, id: &str) -> Self {
            self.fail_ids.insert(id.to_string());
            self
        }
    }

    #[async_trait]
    impl TranslationClient for ControlledClient {
        async fn complete(
            &self,
            system_prompt: &str,
            user_prompt: &str,
        ) -> Result<CompletionOutput, String> {
            assert!(
                system_prompt.contains("TASK:POLISH"),
                "polish round must only issue polish requests"
            );
            let value: serde_json::Value =
                serde_json::from_str(user_prompt).expect("prompt should be JSON");
            let segments = value["segments"]
                .as_array()
                .expect("prompt should contain segments")
                .clone();
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            let slow = segments.iter().any(|segment| {
                self.slow_ids
                    .contains(segment["id"].as_str().unwrap_or_default())
            });
            tokio::time::sleep(Duration::from_millis(if slow {
                self.delay_ms * 4
            } else {
                self.delay_ms
            }))
            .await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            let mut translations = Vec::new();
            for segment in segments.iter() {
                let id = segment["id"].as_str().unwrap_or_default().to_string();
                if self.fail_ids.contains(&id) {
                    return Err("planned polish failure".to_string());
                }
                self.processed
                    .lock()
                    .expect("processed mutex")
                    .push(id.clone());
                translations.push(format!(
                    "已润色：{}",
                    segment["translation"].as_str().unwrap_or_default()
                ));
            }
            Ok(CompletionOutput {
                text: serde_json::json!({ "translations": translations }).to_string(),
                usage: Usage::default(),
            })
        }
    }

    fn config(concurrency: usize) -> AppConfig {
        let mut config = AppConfig::default();
        config.general.polish_concurrency = concurrency;
        config.llm.max_retries = 0;
        config
    }

    fn draft_for(id: &str) -> String {
        format!("draft {}", id.trim_start_matches('c').replace("-s", "-"))
    }

    #[test]
    fn plan_batches_keeps_chapters_separate_and_skips_finished_segments() {
        let mut document = document(2, 3);
        document.chapters[0].segments[1].polish_status = Some(PolishStatus::Succeeded);
        document.chapters[1].segments[0].target_before_polish = None;
        let batches = plan_batches("round", &document.chapters, 1);

        assert_eq!(batches.len(), 4);
        assert_eq!(batches[0].chapter_id, "chapter-0");
        assert_eq!(batches[0].segment_ids, vec!["c0-s0"]);
        assert_eq!(batches[1].segment_ids, vec!["c0-s2"]);
        assert_eq!(batches[2].chapter_id, "chapter-1");
        assert_eq!(batches[2].segment_ids, vec!["c1-s1"]);
        assert_eq!(batches[3].segment_ids, vec!["c1-s2"]);
        assert!(batches.iter().all(|batch| batch.id.starts_with("round-b")));
    }

    #[tokio::test]
    async fn parallel_polish_respects_the_concurrency_limit() {
        let mut fixture = fixture(4, 2);
        let client = Arc::new(ControlledClient::new(20));
        let summary = run_round(
            client.clone(),
            &fixture.state_dir,
            &fixture.project,
            &mut fixture.chapters,
            &analysis(),
            &[],
            &config(2),
            false,
        )
        .await
        .expect("polish should complete");

        assert!(summary.finished);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.succeeded, 4);
        assert!(
            client.max_active.load(Ordering::SeqCst) <= 2,
            "in-flight requests must respect polish_concurrency"
        );
        for chapter in &fixture.chapters {
            for segment in &chapter.segments {
                assert!(segment
                    .target
                    .as_deref()
                    .is_some_and(|value| value.starts_with("已润色：")));
                assert_eq!(
                    segment.target_before_polish.as_deref(),
                    Some(draft_for(&segment.id).as_str())
                );
                assert_eq!(segment.polish_status, Some(PolishStatus::Succeeded));
            }
        }
        std::fs::remove_dir_all(fixture.dir).expect("temp directory should be removed");
    }

    #[tokio::test]
    async fn out_of_order_results_are_applied_in_book_order() {
        let mut fixture = fixture(2, 1);
        let client = Arc::new(ControlledClient::new(10).slow("c0-s0"));
        run_round(
            client.clone(),
            &fixture.state_dir,
            &fixture.project,
            &mut fixture.chapters,
            &analysis(),
            &[],
            &config(2),
            false,
        )
        .await
        .expect("polish should complete");

        let processed = client.processed.lock().expect("processed mutex").clone();
        assert_eq!(processed, vec!["c1-s0", "c0-s0"]);
        for chapter in &fixture.chapters {
            for segment in &chapter.segments {
                assert!(segment.target.as_deref().is_some_and(
                    |value| value.starts_with(&format!("已润色：{}", draft_for(&segment.id)))
                ));
            }
        }
        std::fs::remove_dir_all(fixture.dir).expect("temp directory should be removed");
    }

    #[tokio::test]
    async fn failed_batch_keeps_the_draft_and_can_be_retried() {
        let mut fixture = fixture(2, 1);
        let failing = Arc::new(ControlledClient::new(0).fail("c1-s0"));
        let summary = run_round(
            failing,
            &fixture.state_dir,
            &fixture.project,
            &mut fixture.chapters,
            &analysis(),
            &[],
            &config(2),
            false,
        )
        .await
        .expect("failed batches should not abort the round");

        assert_eq!(summary.failed, 1);
        assert_eq!(summary.succeeded, 1);
        let failed = &fixture.chapters[1].segments[0];
        assert_eq!(failed.target.as_deref(), Some("draft 1-0"));
        assert_eq!(failed.polish_status, Some(PolishStatus::Failed));
        assert_eq!(
            fixture.chapters[0].segments[0].polish_status,
            Some(PolishStatus::Succeeded)
        );

        let retry = Arc::new(ControlledClient::new(0));
        let summary = run_round(
            retry,
            &fixture.state_dir,
            &fixture.project,
            &mut fixture.chapters,
            &analysis(),
            &[],
            &config(2),
            true,
        )
        .await
        .expect("retry should complete");
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.succeeded, 2);
        assert_eq!(
            fixture.chapters[1].segments[0].polish_status,
            Some(PolishStatus::Succeeded)
        );
        std::fs::remove_dir_all(fixture.dir).expect("temp directory should be removed");
    }

    #[tokio::test]
    async fn interrupted_round_resumes_with_the_same_snapshot() {
        let mut fixture = fixture(1, 2);
        let config = config(1);
        state::write_json_atomic(
            &super::round_path(&fixture.state_dir, &fixture.project.id),
            &PolishRound {
                id: "round-interrupted".to_string(),
                created_at: now(),
                updated_at: now(),
                input_digest: drafts_digest(&fixture.chapters),
                terms_digest: terms_digest(&[]),
                terms: Vec::new(),
                style_guide: vec!["Natural Chinese".to_string()],
                book_synopsis: Some("synopsis".to_string()),
                chapter_digests: BTreeMap::new(),
                batches: vec![
                    PolishBatch {
                        id: "round-interrupted-b000".to_string(),
                        chapter_id: "chapter-0".to_string(),
                        segment_ids: vec!["c0-s0".to_string()],
                        status: PolishBatchStatus::Running,
                        attempts: 1,
                        error: None,
                        elapsed_ms: None,
                    },
                    PolishBatch {
                        id: "round-interrupted-b001".to_string(),
                        chapter_id: "chapter-0".to_string(),
                        segment_ids: vec!["c0-s1".to_string()],
                        status: PolishBatchStatus::Pending,
                        attempts: 0,
                        error: None,
                        elapsed_ms: None,
                    },
                ],
                finished: false,
                last_error: None,
            },
        )
        .expect("round should be written");

        let summary = run_round(
            Arc::new(ControlledClient::new(0)),
            &fixture.state_dir,
            &fixture.project,
            &mut fixture.chapters,
            &analysis(),
            &[],
            &config,
            false,
        )
        .await
        .expect("interrupted round should resume");

        assert_eq!(summary.round_id, "round-interrupted");
        assert_eq!(summary.succeeded, 2);
        assert!(fixture.chapters[0]
            .segments
            .iter()
            .all(|segment| segment.polish_status == Some(PolishStatus::Succeeded)));
        std::fs::remove_dir_all(fixture.dir).expect("temp directory should be removed");
    }

    #[tokio::test]
    async fn cancelled_round_keeps_saved_results_and_resumes() {
        let fixture = fixture(3, 1);
        let client = Arc::new(ControlledClient::new(40));
        let handle = tokio::spawn({
            let state_dir = fixture.state_dir.clone();
            let project = fixture.project.clone();
            let mut chapters = fixture.chapters.clone();
            async move {
                run_round(
                    client,
                    &state_dir,
                    &project,
                    &mut chapters,
                    &analysis(),
                    &[],
                    &config(1),
                    false,
                )
                .await
            }
        });
        tokio::time::sleep(Duration::from_millis(15)).await;
        handle.abort();
        let _ = handle.await;

        let round: PolishRound =
            state::read_json(&round_path(&fixture.state_dir, &fixture.project.id))
                .expect("interrupted round should persist");
        assert!(!round.finished);
        assert!(round.batches.iter().any(|batch| {
            matches!(
                batch.status,
                PolishBatchStatus::Running | PolishBatchStatus::Pending
            )
        }));

        let mut chapters = state::load_chapters(&fixture.state_dir, &fixture.project)
            .expect("chapters should load");
        let summary = run_round(
            Arc::new(ControlledClient::new(0)),
            &fixture.state_dir,
            &fixture.project,
            &mut chapters,
            &analysis(),
            &[],
            &config(2),
            false,
        )
        .await
        .expect("interrupted round should resume");
        assert!(summary.finished);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.succeeded, 3);
        assert!(chapters
            .iter()
            .flat_map(|chapter| &chapter.segments)
            .all(
                |segment| segment.polish_status == Some(PolishStatus::Succeeded)
                    && !segment
                        .target
                        .as_deref()
                        .unwrap_or_default()
                        .starts_with("draft")
            ));
        std::fs::remove_dir_all(fixture.dir).expect("temp directory should be removed");
    }

    #[tokio::test]
    async fn changed_draft_invalidates_the_old_snapshot() {
        let mut fixture = fixture(2, 1);
        run_round(
            Arc::new(ControlledClient::new(0)),
            &fixture.state_dir,
            &fixture.project,
            &mut fixture.chapters,
            &analysis(),
            &[],
            &config(2),
            false,
        )
        .await
        .expect("first round should complete");
        let old_round = read_summary(&fixture.state_dir, &fixture.project.id, &fixture.chapters)
            .expect("summary should load")
            .expect("round should exist")
            .round_id;

        fixture.chapters[1].segments[0].target_before_polish = Some("新初稿".to_string());
        fixture.chapters[1].segments[0].target = Some("新初稿".to_string());
        fixture.chapters[1].segments[0].polish_status = Some(PolishStatus::Pending);
        let summary = run_round(
            Arc::new(ControlledClient::new(0)),
            &fixture.state_dir,
            &fixture.project,
            &mut fixture.chapters,
            &analysis(),
            &[],
            &config(2),
            false,
        )
        .await
        .expect("new round should cover the changed draft");

        assert_ne!(summary.round_id, old_round);
        assert_eq!(summary.total, 1);
        assert!(fixture.chapters[1].segments[0]
            .target
            .as_deref()
            .is_some_and(|value| value.starts_with("已润色：新初稿")));
        std::fs::remove_dir_all(fixture.dir).expect("temp directory should be removed");
    }

    #[tokio::test]
    async fn polish_requires_a_complete_translation() {
        let mut fixture = fixture(1, 1);
        fixture.chapters[0].target_title = None;
        let error = run_round(
            Arc::new(ControlledClient::new(0)),
            &fixture.state_dir,
            &fixture.project,
            &mut fixture.chapters,
            &analysis(),
            &[],
            &config(2),
            false,
        )
        .await
        .expect_err("incomplete translation must be rejected");
        assert!(error.contains("全书翻译尚未完成"));
        std::fs::remove_dir_all(fixture.dir).expect("temp directory should be removed");
    }
}
