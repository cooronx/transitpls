//! 项目生命周期：创建、加载、保存项目状态与失败标记。

use super::chapters::{load_chapters, write_chapter, write_chapter_at};
use super::io::{hash_bytes, hash_file, read_json, timestamp, write_json_atomic};
use super::lock::acquire_project_read_lock;
use super::log::{append_log, append_log_at};
use super::{ExportSnapshot, InitializedProject};
use crate::model::{Chapter, Document, ItemStatus, ProjectState, ProjectStatus};
use std::fs;
use std::path::{Path, PathBuf};

/// 项目目录：`state_dir/{SHA-256}`。
pub fn project_dir(state_dir: &Path, id: &str) -> PathBuf {
    state_dir.join(id)
}

/// 初始化项目目录。
///
/// 相同源文件已有项目时直接返回；否则先写 `.creating/` 下的临时目录，
/// 章节全部写完后整体重命名发布，中途失败不会留下半成品项目。
pub fn initialize(
    state_dir: &Path,
    input: &Path,
    document: &Document,
    max_segment_chars: usize,
) -> Result<InitializedProject, String> {
    let source_hash = hash_file(input)?;
    let project_dir = project_dir(state_dir, &source_hash);
    let project_path = project_dir.join("project.json");
    if project_path.exists() {
        let project: ProjectState = read_json(&project_path)?;
        append_log(state_dir, &project, "init_exists", serde_json::json!({}))?;
        return Ok(InitializedProject {
            project,
            created: false,
        });
    }

    let now = timestamp();
    let project = ProjectState {
        id: source_hash.clone(),
        title: document.metadata.title.clone(),
        source_file: input.to_string_lossy().to_string(),
        source_path: fs::canonicalize(input)
            .unwrap_or_else(|_| input.to_path_buf())
            .to_string_lossy()
            .to_string(),
        source_hash: source_hash.clone(),
        source_language: document.metadata.source_language.clone(),
        target_language: document.metadata.target_language.clone(),
        status: ProjectStatus::Initialized,
        chapters_total: document.chapters.len(),
        chapters_completed: 0,
        created_at: now.clone(),
        updated_at: now,
        max_segment_chars,
    };
    let creating_dir = state_dir.join(".creating");
    fs::create_dir_all(&creating_dir)
        .map_err(|error| format!("failed to create initialization directory: {error}"))?;
    let temporary_dir = creating_dir.join(format!("{}-{}", source_hash, std::process::id()));
    fs::create_dir(&temporary_dir).map_err(|error| {
        format!(
            "failed to create temporary project {}: {error}; remove stale directory and retry",
            temporary_dir.display()
        )
    })?;
    fs::create_dir(temporary_dir.join("chapters"))
        .map_err(|error| format!("failed to create temporary chapters directory: {error}"))?;
    write_json_atomic(&temporary_dir.join("project.json"), &project)?;
    for chapter in &document.chapters {
        write_chapter_at(&temporary_dir, chapter)?;
    }
    append_log_at(
        &temporary_dir.join("logs.txt"),
        "initialized",
        serde_json::json!({ "chapters": project.chapters_total }),
    )?;
    fs::rename(&temporary_dir, &project_dir).map_err(|error| {
        format!(
            "failed to publish initialized project {}: {error}",
            project_dir.display()
        )
    })?;
    Ok(InitializedProject {
        project,
        created: true,
    })
}

/// 按源文件路径（内容哈希）加载项目。
pub fn load_for_source(state_dir: &Path, input: &Path) -> Result<ProjectState, String> {
    let hash = hash_file(input)?;
    load_project(state_dir, &hash)
}

/// 组装导出快照，并校验源文件哈希与章节数量一致。
pub fn load_export_snapshot(state_dir: &Path, input: &Path) -> Result<ExportSnapshot, String> {
    let input_path = fs::canonicalize(input)
        .map_err(|error| format!("failed to resolve input file {}: {error}", input.display()))?;
    let initial_hash = hash_file(&input_path)?;
    let project = match load_project(state_dir, &initial_hash) {
        Ok(project) => project,
        Err(_) => find_project_by_source_path(state_dir, &input_path)?,
    };
    let _lock = acquire_project_read_lock(state_dir, &project)?;
    let project = load_project(state_dir, &project.id)?;
    let source_bytes = fs::read(&input_path).map_err(|error| {
        format!(
            "failed to read input file {}: {error}",
            input_path.display()
        )
    })?;
    let actual_hash = hash_bytes(&source_bytes);
    if actual_hash != project.source_hash {
        return Err(format!(
            "source file hash changed for project {}: expected {}, got {}",
            project.id, project.source_hash, actual_hash
        ));
    }
    let chapters = load_chapters(state_dir, &project)?;
    if chapters.len() != project.chapters_total {
        return Err(format!(
            "project snapshot is incomplete: project.json declares {} chapters but {} chapter files were found",
            project.chapters_total,
            chapters.len()
        ));
    }
    Ok(ExportSnapshot {
        project,
        chapters,
        source_bytes,
    })
}

/// 当源文件已被移动、无法按哈希定位项目时，回退到按记录的原路径查找最近更新的项目。
fn find_project_by_source_path(state_dir: &Path, input: &Path) -> Result<ProjectState, String> {
    let entries = fs::read_dir(state_dir)
        .map_err(|error| format!("failed to read state directory: {error}"))?;
    let mut matches = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path().join("project.json");
        if !path.is_file() {
            continue;
        }
        let Ok(project) = read_json::<ProjectState>(&path) else {
            continue;
        };
        let stored = Path::new(&project.source_path);
        let stored = fs::canonicalize(stored).unwrap_or_else(|_| stored.to_path_buf());
        if stored == input {
            matches.push(project);
        }
    }
    matches
        .into_iter()
        .max_by(|left, right| left.updated_at.cmp(&right.updated_at))
        .ok_or_else(|| {
            format!(
                "no project found for source {}; run init first",
                input.display()
            )
        })
}

/// 按项目 ID 加载 `project.json`，ID 必须是 64 位十六进制。
pub fn load_project(state_dir: &Path, id: &str) -> Result<ProjectState, String> {
    if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("project id must be a 64-character SHA-256 value".to_string());
    }
    let normalized_id = id.to_ascii_lowercase();
    let path = project_dir(state_dir, &normalized_id).join("project.json");
    if !path.exists() {
        return Err(format!("no project found for id {id}; run init first"));
    }
    read_json(&path)
}

/// 保存翻译进度：刷新章节状态、完成章节数和项目状态，再写回项目与全部章节。
pub fn save_progress(
    state_dir: &Path,
    project: &mut ProjectState,
    chapters: &mut [Chapter],
) -> Result<(), String> {
    for chapter in chapters.iter_mut() {
        if chapter
            .segments
            .iter()
            .all(|segment| segment.status == ItemStatus::Translated)
        {
            chapter.status = ItemStatus::Translated;
        }
    }
    let completed = chapters
        .iter()
        .filter(|chapter| {
            chapter
                .segments
                .iter()
                .all(|segment| segment.status == ItemStatus::Translated)
        })
        .count();
    project.chapters_completed = completed;
    project.updated_at = timestamp();
    if project.status != ProjectStatus::Failed {
        project.status = if completed == project.chapters_total {
            ProjectStatus::Translated
        } else {
            ProjectStatus::Translating
        };
    }
    write_json_atomic(
        &project_dir(state_dir, &project.id).join("project.json"),
        project,
    )?;
    for chapter in chapters {
        write_chapter(state_dir, project, chapter)?;
    }
    Ok(())
}

/// 标记项目失败并记录日志；失败状态会保留到下次成功保存进度。
pub fn mark_failed(
    state_dir: &Path,
    project: &mut ProjectState,
    detail: &str,
) -> Result<(), String> {
    project.status = ProjectStatus::Failed;
    project.updated_at = timestamp();
    write_json_atomic(
        &project_dir(state_dir, &project.id).join("project.json"),
        project,
    )?;
    append_log(
        state_dir,
        project,
        "failed",
        serde_json::json!({ "error": detail }),
    )
}

/// 直接写回 `project.json`，不刷新章节状态。
pub fn save_project(state_dir: &Path, project: &ProjectState) -> Result<(), String> {
    write_json_atomic(
        &project_dir(state_dir, &project.id).join("project.json"),
        project,
    )
}
