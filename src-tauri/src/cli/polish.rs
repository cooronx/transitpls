//! `polish` 命令：对已完成的项目执行润色。

use super::args::PolishArgs;
use crate::config::AppConfig;
use crate::state;
use crate::workflow;
use std::path::Path;

pub(super) async fn run(
    args: PolishArgs,
    state_dir: &Path,
    config: &AppConfig,
) -> Result<i32, String> {
    let (project, summary) =
        workflow::run_polish(args.input, args.retry_failed, args.mock, state_dir, config).await?;
    state::append_log(
        state_dir,
        &project,
        "polish_summary",
        serde_json::json!({
            "round_id": summary.round_id,
            "total": summary.total,
            "succeeded": summary.succeeded,
            "failed": summary.failed,
        }),
    )?;
    println!(
        "polished {} of {} batches ({} failed)",
        summary.succeeded, summary.total, summary.failed
    );
    Ok(if summary.failed == 0 { 0 } else { 2 })
}
