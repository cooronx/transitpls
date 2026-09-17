//! 全书审校：按章节分批检查漏译、错译、术语与文风问题，生成审校报告。
//!
//! 目前只有 mock 客户端实现（用于验证报告结构与落盘），真实审校客户端尚未接入。

use crate::{
    model::{Chapter, ProjectState},
    state,
    terms::TermStore,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// 审校问题类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IssueType {
    /// 漏译。
    Omission,
    /// 错译。
    Mistranslation,
    /// 术语不一致。
    Terminology,
    /// 前后不一致。
    Consistency,
    /// 文风偏移。
    Style,
    /// 格式问题。
    Format,
    /// 未翻译。
    Untranslated,
}

/// 问题严重程度。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// 严重，必须处理。
    Critical,
    /// 主要问题。
    Major,
    /// 次要问题。
    Minor,
}

/// 问题对应的原文/译文证据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub source: String,
    pub target: String,
    /// 相关联的段落 ID，便于定位上下文。
    #[serde(default)]
    pub related_segment_ids: Vec<String>,
}

/// 一条审校问题。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub chapter_id: String,
    pub segment_id: String,
    pub issue_type: IssueType,
    pub severity: Severity,
    pub summary: String,
    pub evidence: Evidence,
}

/// 审校消耗的 token 用量。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// 一次审校的完整报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub run_id: String,
    pub project_id: String,
    /// `completed` 或 `partial`（存在失败批次）。
    pub status: String,
    pub summary: serde_json::Value,
    pub issues: Vec<Issue>,
    pub failed_batches: Vec<String>,
    pub usage: Usage,
}

fn write<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    fs::write(
        path,
        serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write {}: {e}", path.display()))
}
fn issue_id(c: &str, s: &str, t: &IssueType) -> String {
    format!("{c}:{s}:{t:?}").to_lowercase()
}

/// 运行审校并写入 `reviews/{run_id}/`，返回报告与目录路径。
///
/// `resume` 指定时复用已有 run ID 续跑；批次失败会记入报告但不中断整体流程。
pub async fn run(
    state_dir: &Path,
    project: &ProjectState,
    chapters: &[Chapter],
    chapter: Option<usize>,
    mock: bool,
    resume: Option<&str>,
    retry_failed: bool,
) -> Result<(Report, PathBuf), String> {
    let selected: Vec<(usize, &Chapter)> = chapters
        .iter()
        .enumerate()
        .filter(|(i, _)| chapter.is_none_or(|wanted| *i == wanted))
        .collect();
    let incomplete = selected
        .iter()
        .filter(|(_, c)| {
            c.segments
                .iter()
                .any(|s| s.target.as_deref().unwrap_or("").trim().is_empty())
        })
        .count();
    let total = selected.len();
    let segments = selected
        .iter()
        .map(|(_, c)| c.segments.len())
        .sum::<usize>();
    let run_id = resume
        .map(str::to_string)
        .unwrap_or_else(|| format!("review-{}", Utc::now().format("%Y%m%dT%H%M%S%.3fZ")));
    let dir = state::project_dir(state_dir, &project.id)
        .join("reviews")
        .join(&run_id);
    fs::create_dir_all(&dir).map_err(|e| format!("create review directory: {e}"))?;
    write(
        &dir.join("config.json"),
        &serde_json::json!({"project_id":project.id,"chapter":chapter,"mock":mock,"retry_failed":retry_failed}),
    )?;
    write(
        &dir.join("input-snapshot.json"),
        &serde_json::json!({"project":project,"chapters":selected.iter().map(|(_,c)| *c).collect::<Vec<_>>() }),
    )?;
    state::append_log(
        state_dir,
        project,
        "review_started",
        serde_json::json!({"run_id":run_id}),
    )?;
    let terms =
        TermStore::open(state::project_dir(state_dir, &project.id).join("terms.db"))?.list()?;
    let mut issues = Vec::new();
    let mut failed = Vec::new();
    for (_ci, ch) in selected {
        for batch in ch.segments.chunks(20) {
            let batch_id = format!(
                "{}:{}",
                ch.id,
                batch.first().map(|s| s.id.as_str()).unwrap_or("empty")
            );
            let result: Result<Vec<Issue>, String> = if mock {
                Ok(batch
                    .iter()
                    .flat_map(|s| {
                        let target = s.target.as_deref().unwrap_or("");
                        let mut found: Vec<(IssueType, Severity, String)> = Vec::new();
                        if target.trim().is_empty() {
                            found.push((
                                IssueType::Untranslated,
                                Severity::Critical,
                                "译文为空".into(),
                            ));
                        }
                        if target == s.source && !s.source.is_empty() {
                            found.push((
                                IssueType::Untranslated,
                                Severity::Major,
                                "译文疑似残留源文".into(),
                            ));
                        }
                        for term in terms
                            .iter()
                            .filter(|t| s.source.contains(&t.source) && !target.contains(&t.target))
                        {
                            found.push((
                                IssueType::Terminology,
                                Severity::Major,
                                format!("术语应译为 {}", term.target),
                            ));
                        }
                        found
                            .into_iter()
                            .map(|(kind, severity, summary)| Issue {
                                id: issue_id(&ch.id, &s.id, &kind),
                                chapter_id: ch.id.clone(),
                                segment_id: s.id.clone(),
                                issue_type: kind,
                                severity,
                                summary,
                                evidence: Evidence {
                                    source: s.source.clone(),
                                    target: target.to_string(),
                                    related_segment_ids: vec![],
                                },
                            })
                            .collect::<Vec<_>>()
                    })
                    .collect())
            } else {
                Err("非 mock review 客户端尚未接入".into())
            };
            match result {
                Ok(mut batch_issues) => {
                    issues.append(&mut batch_issues);
                    state::append_log(
                        state_dir,
                        project,
                        "review_batch_completed",
                        serde_json::json!({"run_id":run_id,"batch":batch_id}),
                    )?;
                }
                Err(error) => {
                    failed.push(format!("{batch_id}: {error}"));
                    state::append_log(
                        state_dir,
                        project,
                        "review_failed",
                        serde_json::json!({"run_id":run_id,"batch":batch_id,"error":error}),
                    )?;
                }
            }
        }
    }
    issues.sort_by(|a, b| a.id.cmp(&b.id));
    issues.dedup_by(|a, b| a.id == b.id);
    let reviewed = segments.saturating_sub(incomplete);
    let report = Report {
        run_id: run_id.clone(),
        project_id: project.id.clone(),
        status: if failed.is_empty() {
            "completed".into()
        } else {
            "partial".into()
        },
        summary: serde_json::json!({"chapters":format!("{}/{}",total,total),"segments":format!("{}/{}",reviewed,segments),"issues":issues.len(),"incomplete_chapters":incomplete}),
        issues,
        failed_batches: failed,
        usage: Usage::default(),
    };
    write(&dir.join("report.json"), &report)?;
    let md = format!(
        "# Review report\n\n- Project: `{}`\n- Status: `{}`\n- Issues: {}\n- Failed batches: {}\n",
        report.project_id,
        report.status,
        report.issues.len(),
        report.failed_batches.len()
    );
    fs::write(dir.join("report.md"), md).map_err(|e| e.to_string())?;
    state::append_log(
        state_dir,
        project,
        "review_completed",
        serde_json::json!({"run_id":run_id,"issues":report.issues.len(),"failed_batches":report.failed_batches.len()}),
    )?;
    Ok((report, dir))
}
