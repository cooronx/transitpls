use crate::model::{Chapter, Document, ItemStatus, ProjectState, ProjectStatus};
use chrono::{SecondsFormat, Utc};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct ProjectLock {
    file: File,
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub struct InitializedProject {
    pub project: ProjectState,
    pub created: bool,
}

#[derive(Debug, Clone)]
pub struct ExportSnapshot {
    pub project: ProjectState,
    pub chapters: Vec<Chapter>,
    pub source_bytes: Vec<u8>,
}

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

pub fn load_for_source(state_dir: &Path, input: &Path) -> Result<ProjectState, String> {
    let hash = hash_file(input)?;
    load_project(state_dir, &hash)
}

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

pub fn load_chapters(state_dir: &Path, project: &ProjectState) -> Result<Vec<Chapter>, String> {
    let dir = project_dir(state_dir, &project.id).join("chapters");
    let mut chapters = Vec::new();
    let entries =
        fs::read_dir(&dir).map_err(|error| format!("failed to read chapters: {error}"))?;
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        chapters.push(read_json(&path)?);
    }
    chapters.sort_by_key(|chapter: &Chapter| {
        chapter
            .id
            .strip_prefix("chapter-")
            .and_then(|id| id.split('-').next())
            .and_then(|ordinal| ordinal.parse::<usize>().ok())
            .unwrap_or(usize::MAX)
    });
    Ok(chapters)
}

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

pub fn append_log(
    state_dir: &Path,
    project: &ProjectState,
    event: &str,
    details: serde_json::Value,
) -> Result<(), String> {
    let path = project_dir(state_dir, &project.id).join("logs.txt");
    append_log_at(&path, event, details)
}

fn append_log_at(path: &Path, event: &str, details: serde_json::Value) -> Result<(), String> {
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

pub fn hash_file(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|error| format!("failed to open input file: {error}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("failed to hash input file: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format_hash(hasher.finalize()))
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format_hash(hasher.finalize())
}

fn format_hash(hash: impl AsRef<[u8]>) -> String {
    hash.as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn project_dir(state_dir: &Path, id: &str) -> PathBuf {
    state_dir.join(id)
}

fn project_lock_path(state_dir: &Path, project_id: &str) -> PathBuf {
    state_dir.join(".locks").join(format!("{project_id}.lock"))
}

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

fn acquire_project_read_lock(
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

pub fn write_chapter(
    state_dir: &Path,
    project: &ProjectState,
    chapter: &Chapter,
) -> Result<(), String> {
    write_chapter_at(&project_dir(state_dir, &project.id), chapter)
}

pub fn save_project(state_dir: &Path, project: &ProjectState) -> Result<(), String> {
    write_json_atomic(
        &project_dir(state_dir, &project.id).join("project.json"),
        project,
    )
}

fn write_chapter_at(project_dir: &Path, chapter: &Chapter) -> Result<(), String> {
    write_json_atomic(
        &project_dir
            .join("chapters")
            .join(format!("{}.json", chapter.id)),
        chapter,
    )
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&contents)
        .map_err(|error| format!("invalid JSON in {}: {error}", path.display()))
}

pub(crate) fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "state path has no parent".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create state directory: {error}"))?;
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("failed to serialize state: {error}"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp = path.with_extension(format!("tmp-{}-{}", std::process::id(), stamp));
    {
        let mut file = File::create(&temp)
            .map_err(|error| format!("failed to create state temp file: {error}"))?;
        file.write_all(&bytes)
            .map_err(|error| format!("failed to write state: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("failed to sync state: {error}"))?;
    }
    if let Err(error) = fs::rename(&temp, path) {
        #[cfg(windows)]
        {
            if path.exists() {
                fs::remove_file(path).map_err(|remove_error| {
                    format!("failed to replace state file ({error}); remove failed: {remove_error}")
                })?;
                fs::rename(&temp, path).map_err(|rename_error| {
                    format!("failed to replace state file: {rename_error}")
                })?;
            } else {
                return Err(format!("failed to atomically replace state file: {error}"));
            }
        }
        #[cfg(not(windows))]
        {
            let _ = fs::remove_file(&temp);
            return Err(format!("failed to atomically replace state file: {error}"));
        }
    }
    Ok(())
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::{acquire_project_lock, hash_file, initialize, load_export_snapshot, save_project};
    use crate::model::{Chapter, Document, DocumentMetadata, ItemStatus};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "transitpls-state-{name}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temp directory should be created");
        path
    }

    fn document() -> Document {
        Document {
            metadata: DocumentMetadata {
                title: "Book".to_string(),
                source_language: "en".to_string(),
                target_language: "zh-CN".to_string(),
                source_format: "txt".to_string(),
            },
            chapters: vec![Chapter {
                id: "chapter-1-test".to_string(),
                title: "Chapter 1".to_string(),
                target_title: None,
                status: ItemStatus::Pending,
                meta: serde_json::json!({}),
                segments: Vec::new(),
            }],
        }
    }

    #[test]
    fn hashes_source_bytes_with_sha256() {
        let dir = temp_dir("hash");
        let source = dir.join("book.txt");
        fs::write(&source, b"abc").expect("source should be written");
        assert_eq!(
            hash_file(&source).expect("hash should be generated"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        fs::remove_dir_all(dir).expect("temp directory should be removed");
    }

    #[test]
    fn publishes_complete_project_from_creating_directory() {
        let dir = temp_dir("initialize");
        let source = dir.join("book.txt");
        let state_dir = dir.join("projects");
        fs::write(&source, b"content").expect("source should be written");

        let initialized =
            initialize(&state_dir, &source, &document(), 1_200).expect("init should succeed");
        let project_dir = state_dir.join(&initialized.project.id);
        assert!(project_dir.join("project.json").is_file());
        assert!(project_dir.join("chapters/chapter-1-test.json").is_file());
        assert!(project_dir.join("logs.txt").is_file());
        assert!(state_dir.join(".creating").is_dir());
        assert_eq!(
            fs::read_dir(state_dir.join(".creating"))
                .expect("creating directory should be readable")
                .count(),
            0
        );
        fs::remove_dir_all(dir).expect("temp directory should be removed");
    }

    #[test]
    fn project_lock_rejects_concurrent_writer_and_releases_on_drop() {
        let dir = temp_dir("lock");
        let source = dir.join("book.txt");
        let state_dir = dir.join("projects");
        fs::write(&source, b"content").expect("source should be written");
        let initialized =
            initialize(&state_dir, &source, &document(), 1_200).expect("init should succeed");

        let first = acquire_project_lock(&state_dir, &initialized.project)
            .expect("first writer should acquire lock");
        assert!(acquire_project_lock(&state_dir, &initialized.project).is_err());
        drop(first);
        acquire_project_lock(&state_dir, &initialized.project)
            .expect("lock should be released when writer exits");

        fs::remove_dir_all(dir).expect("temp directory should be removed");
    }

    #[test]
    fn export_snapshot_reports_changed_source_hash() {
        let dir = temp_dir("export-hash");
        let source = dir.join("book.txt");
        let state_dir = dir.join("projects");
        fs::write(&source, b"original").expect("source should be written");
        let initialized =
            initialize(&state_dir, &source, &document(), 1_200).expect("init should succeed");
        drop(
            acquire_project_lock(&state_dir, &initialized.project)
                .expect("project lock should be created"),
        );
        fs::write(&source, b"changed").expect("source should change");

        let error = load_export_snapshot(&state_dir, &source)
            .expect_err("changed source must not be exported");
        assert!(error.contains("source file hash changed"));
        fs::remove_dir_all(dir).expect("temp directory should be removed");
    }

    #[test]
    fn export_snapshot_rejects_project_chapter_count_mismatch() {
        let dir = temp_dir("export-snapshot");
        let source = dir.join("book.txt");
        let state_dir = dir.join("projects");
        fs::write(&source, b"content").expect("source should be written");
        let initialized =
            initialize(&state_dir, &source, &document(), 1_200).expect("init should succeed");
        drop(
            acquire_project_lock(&state_dir, &initialized.project)
                .expect("project lock should be created"),
        );
        let mut project = initialized.project;
        project.chapters_total = 2;
        save_project(&state_dir, &project).expect("project should be updated");

        let error = load_export_snapshot(&state_dir, &source)
            .expect_err("inconsistent snapshot must not be exported");
        assert!(error.contains("declares 2 chapters"));
        fs::remove_dir_all(dir).expect("temp directory should be removed");
    }
}
