use crate::model::{Chapter, Document, ItemStatus, ProjectState, ProjectStatus};
use chrono::{SecondsFormat, Utc};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const PROJECTS_DIR: &str = "projects";

pub struct InitializedProject {
    pub project: ProjectState,
    pub created: bool,
}

pub fn initialize(
    input: &Path,
    document: &Document,
    max_segment_chars: usize,
) -> Result<InitializedProject, String> {
    let source_hash = hash_file(input)?;
    let project_dir = project_dir(&source_hash);
    fs::create_dir_all(project_dir.join("chapters"))
        .map_err(|error| format!("failed to create project directory: {error}"))?;
    let project_path = project_dir.join("project.json");
    if project_path.exists() {
        let project: ProjectState = read_json(&project_path)?;
        append_log(&project, "init_exists", serde_json::json!({}))?;
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
        source_hash,
        source_language: document.metadata.source_language.clone(),
        target_language: document.metadata.target_language.clone(),
        status: ProjectStatus::Initialized,
        chapters_total: document.chapters.len(),
        chapters_completed: 0,
        created_at: now.clone(),
        updated_at: now,
        max_segment_chars,
    };
    write_json_atomic(&project_path, &project)?;
    for chapter in &document.chapters {
        write_chapter(&project, chapter)?;
    }
    append_log(
        &project,
        "initialized",
        serde_json::json!({ "chapters": project.chapters_total }),
    )?;
    Ok(InitializedProject {
        project,
        created: true,
    })
}

pub fn load_for_source(input: &Path) -> Result<ProjectState, String> {
    let hash = hash_file(input)?;
    let path = project_dir(&hash).join("project.json");
    if !path.exists() {
        return Err(format!(
            "no project found for current source hash; run init first ({hash})"
        ));
    }
    read_json(&path)
}

pub fn load_chapters(project: &ProjectState) -> Result<Vec<Chapter>, String> {
    let dir = project_dir(&project.id).join("chapters");
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

pub fn save_progress(project: &mut ProjectState, chapters: &mut [Chapter]) -> Result<(), String> {
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
    write_json_atomic(&project_dir(&project.id).join("project.json"), project)?;
    for chapter in chapters {
        write_chapter(project, chapter)?;
    }
    Ok(())
}

pub fn mark_failed(project: &mut ProjectState, detail: &str) -> Result<(), String> {
    project.status = ProjectStatus::Failed;
    project.updated_at = timestamp();
    write_json_atomic(&project_dir(&project.id).join("project.json"), project)?;
    append_log(project, "failed", serde_json::json!({ "error": detail }))
}

pub fn append_log(
    project: &ProjectState,
    event: &str,
    details: serde_json::Value,
) -> Result<(), String> {
    let path = project_dir(&project.id).join("logs.txt");
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
    Ok(format!("{:x}", hasher.finalize()))
}

fn project_dir(id: &str) -> PathBuf {
    Path::new(PROJECTS_DIR).join(id)
}

fn write_chapter(project: &ProjectState, chapter: &Chapter) -> Result<(), String> {
    write_json_atomic(
        &project_dir(&project.id)
            .join("chapters")
            .join(format!("{}.json", chapter.id)),
        chapter,
    )
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&contents)
        .map_err(|error| format!("invalid JSON in {}: {error}", path.display()))
}

fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
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
