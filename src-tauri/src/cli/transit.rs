//! `transit` 命令：翻译全书或指定章节。

use super::args::TransitArgs;
use crate::config::AppConfig;
use crate::state;
use crate::workflow;
use std::path::Path;

pub(super) async fn run(
    args: TransitArgs,
    state_dir: &Path,
    config: &AppConfig,
) -> Result<i32, String> {
    let project = workflow::transit(args.input, args.chapter, args.mock, state_dir, config).await?;
    let chapters = state::load_chapters(state_dir, &project)?;
    println!(
        "translated {} chapters ({} segments)",
        project.chapters_completed,
        chapters
            .iter()
            .map(|chapter| chapter.segments.len())
            .sum::<usize>()
    );
    Ok(0)
}
