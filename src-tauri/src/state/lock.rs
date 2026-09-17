//! 项目文件锁：写操作排他，导出只读共享。

use super::io::timestamp;
use super::ProjectLock;
use crate::model::ProjectState;
use fs2::FileExt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

fn project_lock_path(state_dir: &Path, project_id: &str) -> PathBuf {
    state_dir.join(".locks").join(format!("{project_id}.lock"))
}

/// 获取项目写锁；同一项目同时只允许一个写任务，冲突时报错并写入持有者信息。
pub fn acquire_project_lock(
    state_dir: &Path,
    project: &ProjectState,
) -> Result<ProjectLock, String> {
    let locks_dir = state_dir.join(".locks");
    fs::create_dir_all(&locks_dir).map_err(|error| {
        format!(
            "failed to create project lock directory {}: {error}",
            locks_dir.display()
        )
    })?;
    let path = project_lock_path(state_dir, &project.id);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| format!("failed to open project lock {}: {error}", path.display()))?;
    file.try_lock_exclusive().map_err(|error| {
        format!(
            "project {} is already being modified by another command: {error}",
            project.id
        )
    })?;
    file.set_len(0)
        .map_err(|error| format!("failed to reset project lock metadata: {error}"))?;
    writeln!(
        file,
        "pid={} acquired_at={}",
        std::process::id(),
        timestamp()
    )
    .map_err(|error| format!("failed to write project lock metadata: {error}"))?;
    file.sync_data()
        .map_err(|error| format!("failed to flush project lock metadata: {error}"))?;
    Ok(ProjectLock { file })
}

/// 获取只读共享锁，用于导出时确认没有写任务正在修改项目。
pub(super) fn acquire_project_read_lock(
    state_dir: &Path,
    project: &ProjectState,
) -> Result<ProjectLock, String> {
    let path = project_lock_path(state_dir, &project.id);
    let file = OpenOptions::new().read(true).open(&path).map_err(|error| {
        format!(
            "project lock {} is unavailable; run transit before export: {error}",
            path.display()
        )
    })?;
    FileExt::try_lock_shared(&file).map_err(|error| {
        format!(
            "project {} is already being modified by another command: {error}",
            project.id
        )
    })?;
    Ok(ProjectLock { file })
}
