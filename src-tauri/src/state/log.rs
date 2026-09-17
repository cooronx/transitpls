//! 项目运行日志 `logs.txt`，每行一条「时间 + 事件 + JSON 详情」。

use super::io::timestamp;
use super::project_dir;
use crate::model::ProjectState;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// 向项目的 `logs.txt` 追加一条事件。
pub fn append_log(
    state_dir: &Path,
    project: &ProjectState,
    event: &str,
    details: serde_json::Value,
) -> Result<(), String> {
    let path = project_dir(state_dir, &project.id).join("logs.txt");
    append_log_at(&path, event, details)
}

/// 按日志文件路径追加事件，初始化期间项目目录尚未发布时使用。
pub(crate) fn append_log_at(
    path: &Path,
    event: &str,
    details: serde_json::Value,
) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("failed to open project log: {error}"))?;
    writeln!(file, "{}\t{}\t{}", timestamp(), event, details)
        .map_err(|error| format!("failed to append project log: {error}"))?;
    file.sync_data()
        .map_err(|error| format!("failed to flush project log: {error}"))
}
