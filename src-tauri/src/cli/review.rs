//! `review` 命令：运行审校并输出报告。

use super::args::ReviewArgs;
use crate::review;
use crate::state;
use std::path::Path;

pub(super) async fn run(args: ReviewArgs, state_dir: &Path) -> Result<i32, String> {
    let project = if let Some(id) = args.project {
        state::load_project(state_dir, &id)?
    } else if let Some(input) = args.input {
        state::load_for_source(state_dir, &input)?
    } else {
        return Err("review requires INPUT or --project".into());
    };
    let chapters = state::load_chapters(state_dir, &project)?;
    if let Some(index) = args.chapter {
        if index >= chapters.len() {
            return Err(format!("chapter index {index} is out of range"));
        }
    }
    let (report, dir) = review::run(
        state_dir,
        &project,
        &chapters,
        args.chapter,
        args.mock,
        args.resume.as_deref(),
        args.retry_failed,
    )
    .await?;
    if let Some(out) = args.out {
        std::fs::copy(
            if args.format == "json" {
                dir.join("report.json")
            } else {
                dir.join("report.md")
            },
            &out,
        )
        .map_err(|e| format!("copy report: {e}"))?;
    }
    if args.format == "json" {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "review completed: project {}\nrun: {}\nissues: {}\nreport: {}",
            project.id,
            dir.display(),
            report.issues.len(),
            dir.join("report.md").display()
        );
    }
    Ok(if report.failed_batches.is_empty() {
        0
    } else {
        2
    })
}
