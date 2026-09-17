//! 命令行参数定义。

use crate::export::ExportFormat;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "transitpls-cli",
    version,
    about = "Long-form document translation workflow"
)]
pub struct Cli {
    /// 自定义配置文件路径，默认使用 `transitpls.toml`。
    #[arg(long, global = true)]
    pub(crate) config: Option<PathBuf>,
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// 初始化项目并运行全书分析。
    Init(InitArgs),
    /// 翻译全书或指定章节。
    Transit(TransitArgs),
    /// 对已完成的项目执行润色。
    Polish(PolishArgs),
    /// 运行审校并输出报告。
    Review(ReviewArgs),
    /// 导出 TXT 或 EPUB。
    Export(ExportArgs),
    /// 查看项目状态。
    Status(ProjectArgs),
    /// 查看与裁定术语。
    Terms(TermsArgs),
}

#[derive(Debug, Args)]
pub(crate) struct InitArgs {
    pub(crate) input: PathBuf,
    #[arg(long)]
    pub(crate) source_language: Option<String>,
    #[arg(long)]
    pub(crate) max_segment_chars: Option<usize>,
    #[arg(long)]
    pub(crate) mock: bool,
    #[arg(long)]
    pub(crate) force_analysis: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TransitArgs {
    pub(crate) input: PathBuf,
    /// 只翻译指定章节（从 0 开始）。
    #[arg(long)]
    pub(crate) chapter: Option<usize>,
    #[arg(long)]
    pub(crate) mock: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PolishArgs {
    pub(crate) input: PathBuf,
    /// 重试上一轮失败的批次。
    #[arg(long)]
    pub(crate) retry_failed: bool,
    #[arg(long)]
    pub(crate) mock: bool,
}

#[derive(Debug, Args)]
pub(crate) struct InputArgs {
    pub(crate) input: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct ReviewArgs {
    pub(crate) input: Option<PathBuf>,
    #[arg(long)]
    pub(crate) project: Option<String>,
    #[arg(long)]
    pub(crate) chapter: Option<usize>,
    #[arg(long)]
    pub(crate) severity: Option<String>,
    #[arg(long, default_value = "text")]
    pub(crate) format: String,
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
    #[arg(long)]
    pub(crate) mock: bool,
    #[arg(long)]
    pub(crate) resume: Option<String>,
    #[arg(long)]
    pub(crate) retry_failed: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ProjectArgs {
    #[arg(value_name = "INPUT", required_unless_present = "project")]
    pub(crate) input: Option<PathBuf>,
    #[arg(long, value_name = "SHA256", required_unless_present = "input")]
    pub(crate) project: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ExportArgs {
    #[arg(long)]
    pub(crate) format: ExportFormatArg,
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
    pub(crate) input: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ExportFormatArg {
    Txt,
    Epub,
}

impl From<ExportFormatArg> for ExportFormat {
    fn from(value: ExportFormatArg) -> Self {
        match value {
            ExportFormatArg::Txt => Self::Txt,
            ExportFormatArg::Epub => Self::Epub,
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct TermsArgs {
    #[command(subcommand)]
    pub(crate) command: TermsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TermsCommand {
    /// 列出全部术语。
    List(ProjectArgs),
    /// 列出待裁定的译名冲突。
    Conflicts(ProjectArgs),
    /// 人工裁定某个术语的译名。
    Resolve(TermsResolveArgs),
}

#[derive(Debug, Args)]
pub(crate) struct TermsResolveArgs {
    #[arg(long, value_name = "SHA256")]
    pub(crate) project: Option<String>,
    #[arg(value_name = "INPUT SOURCE TARGET", num_args = 2..=3)]
    pub(crate) values: Vec<String>,
}
