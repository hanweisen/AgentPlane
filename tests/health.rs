mod common;

use common::*;

#[test]
fn cli_health_remains_available_without_token() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;

    let health = run_cli(&["health", "--server", &harness.base_url])?;
    assert!(health.status.success());
    Ok(())
}

#[test]
fn cli_health_token_sends_authorization_header() -> Result<()> {
    let token = "test-token";
    let harness = HeaderGateHarness::start("authorization", "Bearer test-token")?;

    let health = run_cli(&["health", "--server", &harness.base_url, "--token", token])?;
    assert!(health.status.success());
    Ok(())
}
