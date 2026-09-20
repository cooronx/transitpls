//! 译文与版本记录一起保存在章节文件中，调用方持有项目锁后原子写回。

use crate::model::{Chapter, ItemStatus, PolishStatus, Segment, SegmentKind};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RevisionKind {
    Translation,
    Polish,
    Retranslation,
    Manual,
    Restore,
    Adopt,
    Legacy,
    LegacyDraft,
    LegacyPrevious,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Revision {
    pub id: u64,
    pub target: String,
    pub kind: RevisionKind,
    pub created_at: Option<String>,
    pub model: Option<String>,
}

pub fn is_protected(segment: &Segment) -> bool {
    segment.meta["translation_protected"].as_bool() == Some(true)
}

/// 旧项目只展示确实保留下来的文本，未知时间和模型保持为空。
pub fn history(segment: &Segment) -> Result<Vec<Revision>, String> {
    if let Some(value) = segment.meta.get("translation_revisions") {
        return serde_json::from_value(value.clone())
            .map_err(|error| format!("译文历史无法读取：{error}"));
    }
    let mut values = Vec::new();
    let mut add = |target: Option<&str>, kind| {
        if let Some(target) = target.filter(|value| !value.trim().is_empty()) {
            if values
                .iter()
                .any(|revision: &Revision| revision.target == target)
            {
                return;
            }
            values.push(Revision {
                id: values.len() as u64 + 1,
                target: target.to_string(),
                kind,
                created_at: None,
                model: None,
            });
        }
    };
    let current = segment.target.as_deref();
    add(
        segment.meta["previous_target"]
            .as_str()
            .filter(|value| Some(*value) != current),
        RevisionKind::LegacyPrevious,
    );
    add(
        segment
            .target_before_polish
            .as_deref()
            .filter(|value| Some(*value) != current),
        RevisionKind::LegacyDraft,
    );
    add(current, RevisionKind::Legacy);
    Ok(values)
}

pub fn current_revision(segment: &Segment) -> Result<u64, String> {
    Ok(history(segment)?.last().map_or(0, |revision| revision.id))
}

pub fn check_version(
    segment: &Segment,
    expected_revision: u64,
    expected_target: &str,
) -> Result<(), String> {
    if current_revision(segment)? != expected_revision
        || segment.target.as_deref() != Some(expected_target)
    {
        return Err("译文已被其他操作修改，请刷新当前译文并重新比较；你的草稿仍保留。".into());
    }
    Ok(())
}

/// 所有正文写入共用此入口；不改变内容时不创建版本或改变人工保护状态。
pub fn set_target(
    segment: &mut Segment,
    target: String,
    kind: RevisionKind,
    model: Option<&str>,
) -> Result<bool, String> {
    if target.trim().is_empty() {
        return Err("译文不能为空".into());
    }
    if segment.target.as_deref() == Some(target.as_str()) {
        return Ok(false);
    }
    if is_protected(segment)
        && matches!(
            kind,
            RevisionKind::Translation | RevisionKind::Polish | RevisionKind::Retranslation
        )
    {
        return Err("此段已人工修改，请在段落操作中预览重译并确认采用".into());
    }
    let mut revisions = history(segment)?;
    revisions.push(Revision {
        id: revisions.last().map_or(1, |revision| revision.id + 1),
        target: target.clone(),
        kind,
        created_at: Some(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)),
        model: model.map(str::to_string),
    });
    if !segment.meta.is_object() {
        segment.meta = serde_json::json!({});
    }
    if let Some(previous) = &segment.target {
        segment.meta["previous_target"] = serde_json::json!(previous);
    }
    segment.meta["translation_revisions"] =
        serde_json::to_value(revisions).map_err(|error| error.to_string())?;
    if matches!(
        kind,
        RevisionKind::Manual | RevisionKind::Restore | RevisionKind::Adopt
    ) {
        segment.meta["translation_protected"] = serde_json::json!(true);
        // 后续上下文引用人工定稿；旧润色快照由写入方作废。
        segment.target_before_polish = Some(target.clone());
        segment.polish_status = None;
    }
    segment.target = Some(target);
    segment.status = ItemStatus::Translated;
    segment
        .meta
        .as_object_mut()
        .unwrap()
        .remove("retranslation_error");
    Ok(true)
}

/// 更新同名章节标题及其正文标题，避免导出目录与编辑后的标题不一致。
pub fn sync_title(
    chapter: &mut Chapter,
    target: &str,
    kind: RevisionKind,
    model: Option<&str>,
) -> Result<(), String> {
    chapter.target_title = Some(target.to_string());
    for segment in &mut chapter.segments {
        if segment.kind == SegmentKind::Heading && segment.source.trim() == chapter.title.trim() {
            set_target(segment, target.to_string(), kind, model)?;
            if !is_protected(segment) {
                segment.target_before_polish = None;
                segment.polish_status = None;
            }
        }
    }
    Ok(())
}

pub fn eligible_for_polish(segment: &Segment) -> bool {
    !is_protected(segment)
        && segment.polish_status != Some(PolishStatus::Succeeded)
        && segment
            .target_before_polish
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment() -> Segment {
        serde_json::from_value(serde_json::json!({
            "id":"s", "ordinal":0, "source":"source", "target":"旧译文",
            "target_before_polish":"草稿", "kind":"paragraph", "status":"translated", "source_hash":"hash",
            "meta":{"previous_target":"更早译文"}
        })).unwrap()
    }

    #[test]
    fn legacy_history_and_restore_preserve_known_versions_without_inventing_metadata() {
        let mut segment = segment();
        let initial = history(&segment).unwrap();
        assert_eq!(initial.len(), 3);
        assert!(initial
            .iter()
            .all(|revision| revision.created_at.is_none() && revision.model.is_none()));
        assert!(!set_target(&mut segment, "旧译文".into(), RevisionKind::Manual, None).unwrap());
        set_target(&mut segment, "人工译文".into(), RevisionKind::Manual, None).unwrap();
        assert!(is_protected(&segment));
        assert!(!eligible_for_polish(&segment));
        assert_eq!(segment.target_before_polish.as_deref(), Some("人工译文"));
        set_target(
            &mut segment,
            initial[0].target.clone(),
            RevisionKind::Restore,
            None,
        )
        .unwrap();
        let saved = history(&segment).unwrap();
        assert_eq!(saved.len(), 5);
        assert_eq!(saved[3].target, "人工译文");
        assert_eq!(saved[4].kind, RevisionKind::Restore);
    }

    #[test]
    fn version_check_detects_change_away_and_back_and_blank_edits_are_rejected() {
        let mut segment = segment();
        let revision = current_revision(&segment).unwrap();
        set_target(
            &mut segment,
            "新译文".into(),
            RevisionKind::Retranslation,
            Some("model-a"),
        )
        .unwrap();
        set_target(&mut segment, "旧译文".into(), RevisionKind::Restore, None).unwrap();
        assert!(check_version(&segment, revision, "旧译文").is_err());
        let count = history(&segment).unwrap().len();
        assert!(set_target(&mut segment, " \n".into(), RevisionKind::Manual, None).is_err());
        assert_eq!(history(&segment).unwrap().len(), count);
        assert_eq!(
            history(&segment).unwrap()[3].model.as_deref(),
            Some("model-a")
        );
    }
}
