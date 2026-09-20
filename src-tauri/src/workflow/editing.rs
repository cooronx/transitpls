//! 段落编辑、历史恢复与重译预览。模型建议在采用前不修改当前译文。

use super::common::{build_client, load_translation_analysis, term_store};
use crate::{
    config::AppConfig,
    model::{Segment, SegmentKind},
    polish,
    revisions::{self, Revision, RevisionKind},
    state,
};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetranslationPreview {
    pub id: String,
    pub target: String,
    pub base_revision: u64,
    pub base_target: String,
    pub model: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SegmentDetail {
    pub segment: Segment,
    pub revisions: Vec<Revision>,
    pub revision: u64,
    pub preview: Option<RetranslationPreview>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Edit {
    Manual { target: String },
    Restore { revision_id: u64 },
    Adopt { preview_id: String },
}

#[derive(Debug, Deserialize)]
pub struct EditRequest {
    pub project_id: String,
    pub segment_id: String,
    pub expected_revision: u64,
    pub expected_target: String,
    pub edit: Edit,
}

fn detail(segment: &Segment) -> Result<SegmentDetail, String> {
    Ok(SegmentDetail {
        segment: segment.clone(),
        revisions: revisions::history(segment)?,
        revision: revisions::current_revision(segment)?,
        preview: segment
            .meta
            .get("retranslation_preview")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .map_err(|error| format!("重译预览无法读取：{error}"))?,
    })
}

fn position(chapters: &[crate::model::Chapter], id: &str) -> Result<(usize, usize), String> {
    chapters
        .iter()
        .enumerate()
        .find_map(|(ci, chapter)| {
            chapter
                .segments
                .iter()
                .position(|segment| segment.id == id)
                .map(|si| (ci, si))
        })
        .ok_or_else(|| "段落不存在，请重新打开项目".into())
}

pub fn read_segment(
    state_dir: &Path,
    project_id: &str,
    segment_id: &str,
) -> Result<SegmentDetail, String> {
    let project = state::load_project(state_dir, project_id)?;
    let chapters = state::load_chapters(state_dir, &project)?;
    let (ci, si) = position(&chapters, segment_id)?;
    detail(&chapters[ci].segments[si])
}

pub fn save_segment(state_dir: &Path, request: EditRequest) -> Result<SegmentDetail, String> {
    let project = state::load_project(state_dir, &request.project_id)?;
    let _lock = state::acquire_project_lock(state_dir, &project)
        .map_err(|error| format!("项目有任务运行中，暂不能保存修改：{error}"))?;
    let mut chapters = state::load_chapters(state_dir, &project)?;
    let (ci, si) = position(&chapters, &request.segment_id)?;
    let chapter_title = chapters[ci].title.clone();
    let segment = &mut chapters[ci].segments[si];
    revisions::check_version(segment, request.expected_revision, &request.expected_target)?;
    let (target, kind, model) = match request.edit {
        Edit::Manual { target } => (target, RevisionKind::Manual, None),
        Edit::Restore { revision_id } => {
            let old = revisions::history(segment)?
                .into_iter()
                .find(|revision| revision.id == revision_id)
                .ok_or("历史版本不存在")?;
            (old.target, RevisionKind::Restore, None)
        }
        Edit::Adopt { preview_id } => {
            let preview = detail(segment)?
                .preview
                .filter(|preview| preview.id == preview_id)
                .ok_or("重译建议不存在或已更新，请重新预览")?;
            revisions::check_version(segment, preview.base_revision, &preview.base_target)?;
            (preview.target, RevisionKind::Adopt, preview.model)
        }
    };
    if !revisions::set_target(segment, target.clone(), kind, model.as_deref())? {
        return detail(segment);
    }
    segment
        .meta
        .as_object_mut()
        .unwrap()
        .remove("retranslation_preview");
    let is_title =
        segment.kind == SegmentKind::Heading && segment.source.trim() == chapter_title.trim();
    if is_title {
        revisions::sync_title(&mut chapters[ci], &target, kind, model.as_deref())?;
    }
    // 先使旧快照失效；章节写入失败时旧译文和历史仍完整，最多重新规划润色。
    polish::invalidate_round(state_dir, &project.id)?;
    state::write_chapter(state_dir, &project, &chapters[ci])?;
    let _ = state::append_log(
        state_dir,
        &project,
        "translation_edited",
        serde_json::json!({"segment_id": request.segment_id, "kind": kind}),
    );
    detail(&chapters[ci].segments[si])
}

pub async fn preview_retranslation(
    state_dir: &Path,
    config: &AppConfig,
    project_id: &str,
    segment_id: &str,
) -> Result<SegmentDetail, String> {
    let project = state::load_project(state_dir, project_id)?;
    let client = build_client(config, false, state_dir, &project)?;
    preview_with_client(state_dir, config, &project, segment_id, client.as_ref()).await
}

async fn preview_with_client(
    state_dir: &Path,
    config: &AppConfig,
    project: &crate::model::ProjectState,
    segment_id: &str,
    client: &dyn crate::llm::TranslationClient,
) -> Result<SegmentDetail, String> {
    let _lock = state::acquire_project_lock(state_dir, project)?;
    let mut chapters = state::load_chapters(state_dir, project)?;
    let (ci, si) = position(&chapters, segment_id)?;
    let base_target = chapters[ci].segments[si]
        .target
        .clone()
        .ok_or("请先完成此段翻译")?;
    let base_revision = revisions::current_revision(&chapters[ci].segments[si])?;
    let analysis = load_translation_analysis(state_dir, project, &chapters, config)?;
    let store = term_store(state_dir, project)?;
    let segment = &chapters[ci].segments[si];
    let terms = store.relevant(&segment.source)?;
    let recent =
        crate::pipeline::recent_targets(&chapters, ci, si, config.pipeline.recent_context_chars);
    let surrounding = crate::pipeline::surrounding_source(&chapters[ci].segments, si..si + 1);
    let target = super::transit::request_translation(
        client,
        std::slice::from_ref(segment),
        project,
        &analysis,
        chapters[ci].meta["source_digest"].as_str(),
        &terms,
        &recent,
        &surrounding,
        config.llm.max_retries,
    )
    .await?
    .remove(0);
    let preview = RetranslationPreview {
        id: format!("{:032x}", rand::random::<u128>()),
        target,
        base_revision,
        base_target,
        model: client.model_name().map(str::to_string),
    };
    let segment = &mut chapters[ci].segments[si];
    if !segment.meta.is_object() {
        segment.meta = serde_json::json!({});
    }
    segment.meta["retranslation_preview"] =
        serde_json::to_value(preview).map_err(|error| error.to_string())?;
    state::write_chapter(state_dir, project, &chapters[ci])?;
    detail(&chapters[ci].segments[si])
}

#[cfg(test)]
mod tests;
