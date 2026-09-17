//! 本地项目状态的持久化层。
//!
//! 每个项目对应 `state_dir/{项目ID}/` 下的 `project.json`、`chapters/*.json` 和
//! `logs.txt`；所有写入都通过临时文件原子替换，跨进程并发由 `.locks/` 下的文件锁限制。

mod chapters;
mod io;
mod lock;
mod log;
mod project;

#[cfg(test)]
mod tests;

pub use chapters::{load_chapters, normalize_polish_state, write_chapter};
pub use io::hash_file;
pub(crate) use io::{read_json, write_json_atomic};
pub use lock::acquire_project_lock;
pub use log::append_log;
pub(crate) use log::append_log_at;
pub use project::{
    initialize, load_export_snapshot, load_for_source, load_project, mark_failed, project_dir,
    save_progress, save_project,
};

use crate::model::{Chapter, ProjectState};
use std::fs::File;

/// 项目级排他文件锁，写操作期间持有，Drop 时自动释放。
pub struct ProjectLock {
    file: File,
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

/// `initialize` 的返回值：项目状态以及是否为本次新建。
pub struct InitializedProject {
    pub project: ProjectState,
    pub created: bool,
}

/// 导出所需的一致快照：项目状态、全部章节和源文件字节。
#[derive(Debug, Clone)]
pub struct ExportSnapshot {
    pub project: ProjectState,
    pub chapters: Vec<Chapter>,
    pub source_bytes: Vec<u8>,
}
