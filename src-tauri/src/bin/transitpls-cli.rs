//! 命令行入口：执行 `transitpls-cli` 并以其退出码结束进程。

#[tokio::main]
async fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(transitpls_lib::cli::run().await as u8)
}
