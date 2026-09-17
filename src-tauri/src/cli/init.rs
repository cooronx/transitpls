//! `init` 命令：初始化项目并运行全书分析。

use super::args::InitArgs;
use crate::workflow;
use std::path::PathBuf;

pub(super) async fn run(config_path: Option<PathBuf>, args: InitArgs) -> Result<i32, String> {
    let (project, created) = workflow::initialize_project(
        config_path,
        args.input,
        args.source_language,
        args.max_segment_chars,
        args.mock,
        args.force_analysis,
    )
    .await?;
    println!(
        "{} project {} ({} chapters, source language {}, target language {})",
        if created { "initialized" } else { "prepared" },
        project.id,
        project.chapters_total,
        project.source_language,
        project.target_language
    );
    Ok(0)
}
