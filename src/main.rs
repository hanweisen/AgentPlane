use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    match agentplane::cli::run().await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::from(1)
        }
    }
}
