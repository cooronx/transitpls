//! `status` 命令：加载项目并打印进度。

use super::args::ProjectArgs;
use crate::model::{Chapter, ItemStatus, ProjectState};
use crate::state;
use std::path::Path;

pub(super) fn run(args: ProjectArgs, state_dir: &Path) -> Result<i32, String> {
    let project = load_project_args(state_dir, &args)?;
    let chapters = state::load_chapters(state_dir, &project)?;
    state::append_log(
        state_dir,
        &project,
        "status_requested",
        serde_json::json!({}),
    )?;
    print_status(&project, &chapters);
    Ok(0)
}

/// 按文件或 `--project` 定位项目。
pub(super) fn load_project_args(
    state_dir: &Path,
    args: &ProjectArgs,
) -> Result<ProjectState, String> {
    match (&args.input, &args.project) {
        (Some(input), None) => state::load_for_source(state_dir, input),
        (None, Some(id)) => state::load_project(state_dir, id),
        _ => Err("provide either an input file or --project <sha256>".to_string()),
    }
}

fn print_status(project: &ProjectState, chapters: &[Chapter]) {
    println!("project: {}", project.id);
    println!("title: {}", project.title);
    println!("status: {:?}", project.status);
    println!("source: {}", project.source_file);
    println!(
        "languages: {} -> {}",
        project.source_language, project.target_language
    );
    println!(
        "chapters: {}/{} completed",
        project.chapters_completed, project.chapters_total
    );
    for (index, chapter) in chapters.iter().enumerate() {
        let translated = chapter
            .segments
            .iter()
            .filter(|segment| segment.status == ItemStatus::Translated)
            .count();
        println!(
            "  {}. {} [{:?}] {}/{} segments",
            index + 1,
            chapter.title,
            chapter.status,
            translated,
            chapter.segments.len()
        );
    }
}
