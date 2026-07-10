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

#[test]
fn cli_health_profile_label_surfaces_in_output() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let profile_dir = tempfile::tempdir()?;
    let profile_path = profile_dir.path().join("node13.env");
    std::fs::write(
        &profile_path,
        format!(
            "AP_SERVER={}\nAP_TOKEN={token}\nAP_LABEL=node13\n",
            harness.base_url
        ),
    )?;

    let health = run_cli(&["--profile", &profile_path.display().to_string(), "health"])?;
    assert!(
        health.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&health.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&health.stdout)?;
    assert_eq!(body["ok"], true);
    assert_eq!(body["label"], "node13");
    assert_eq!(body["server"], harness.base_url);

    // --label overrides the profile label without changing ok semantics.
    let overridden = run_cli(&[
        "--profile",
        &profile_path.display().to_string(),
        "health",
        "--label",
        "node13-dr",
    ])?;
    assert!(overridden.status.success());
    let overridden_body: serde_json::Value = serde_json::from_slice(&overridden.stdout)?;
    assert_eq!(overridden_body["label"], "node13-dr");
    assert_eq!(overridden_body["server"], harness.base_url);

    Ok(())
}

#[test]
fn cli_health_socks_failure_hints_at_proxy() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;

    // A SOCKS proxy on a port nothing listens on forces a connect error. The
    // error must surface the configured proxy address so the agent can check it.
    let health = run_cli(&[
        "health",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--socks5-hostname",
        "127.0.0.1:1",
        "--connect-retries",
        "0",
    ])?;
    assert!(!health.status.success());
    let stderr = String::from_utf8(health.stderr)?;
    assert!(
        stderr.contains("127.0.0.1:1") && stderr.contains("SOCKS proxy"),
        "missing SOCKS hint in error: {stderr}"
    );
    Ok(())
}
