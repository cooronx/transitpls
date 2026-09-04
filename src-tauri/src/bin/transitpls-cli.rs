#[tokio::main]
async fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(transitpls_lib::cli::run().await as u8)
}
