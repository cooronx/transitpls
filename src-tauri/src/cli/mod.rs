//! 命令行入口：解析参数、分发到 workflow 流程，并负责终端输出。

mod args;
mod export;
mod init;
mod polish;
mod review;
mod status;
mod terms;
mod transit;

#[cfg(test)]
mod tests;

use crate::config;
use args::Command;
use clap::Parser;

pub use args::Cli;

/// 解析参数并执行对应命令，返回进程退出码。
pub async fn run() -> i32 {
    let cli = Cli::parse();
    match execute(cli).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    }
}

async fn execute(cli: Cli) -> Result<i32, String> {
    let config_path = cli.config.clone();
    let loaded = config::load(config_path.as_deref())?;
    let state_dir = loaded.state_dir;
    let config = loaded.value;
    match cli.command {
        Command::Init(args) => init::run(config_path, args).await,
        Command::Status(args) => status::run(args, &state_dir),
        Command::Transit(args) => transit::run(args, &state_dir, &config).await,
        Command::Polish(args) => polish::run(args, &state_dir, &config).await,
        Command::Review(args) => review::run(args, &state_dir).await,
        Command::Export(args) => export::run(args, &state_dir),
        Command::Terms(args) => terms::run(args, &state_dir),
    }
}
