mod common;

use agentplane::git::{
    collect_repo_changes, collect_repo_snapshot, collect_repo_worktree_snapshot, parse_env_pairs,
    resolve_ref,
};
use agentplane::protocol::{SyncMode, SyncPayload};
use agentplane::server::{ServerState, handle_sync_run};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use common::*;

#[test]
fn parse_env_pairs_rejects_invalid_input() {
    assert!(parse_env_pairs(&["A=1".to_string(), "B=2".to_string()]).is_ok());
    assert!(parse_env_pairs(&["broken".to_string()]).is_err());
}

#[test]
fn collect_repo_changes_handles_modified_untracked_and_deleted() -> Result<()> {
    let repo = init_repo()?;
    std::fs::write(repo.path().join(".gitignore"), "ignored.tmp\n")?;
    std::fs::write(repo.path().join("tracked.txt"), "old\n")?;
    std::fs::write(repo.path().join("delete-me.txt"), "bye\n")?;
    git(
        repo.path(),
        &["add", ".gitignore", "tracked.txt", "delete-me.txt"],
    )?;
    git(repo.path(), &["commit", "-m", "init"])?;

    std::fs::write(repo.path().join("tracked.txt"), "new\n")?;
    std::fs::write(repo.path().join("new.txt"), "new file\n")?;
    std::fs::write(repo.path().join("ignored.tmp"), "ignore\n")?;
    std::fs::remove_file(repo.path().join("delete-me.txt"))?;

    let (writes, deletes) = collect_repo_changes(repo.path())?;
    let paths = writes.into_iter().map(|item| item.path).collect::<Vec<_>>();
    assert!(paths.contains(&"tracked.txt".to_string()));
    assert!(paths.contains(&"new.txt".to_string()));
    assert!(!paths.contains(&"ignored.tmp".to_string()));
    assert_eq!(deletes, vec!["delete-me.txt".to_string()]);
    Ok(())
}

#[test]
fn collect_repo_worktree_snapshot_uses_current_project_files() -> Result<()> {
    let repo = init_repo()?;
    std::fs::write(repo.path().join(".gitignore"), "ignored.tmp\n")?;
    std::fs::write(repo.path().join("tracked.txt"), "old\n")?;
    std::fs::write(repo.path().join("deleted.txt"), "delete\n")?;
    git(
        repo.path(),
        &["add", ".gitignore", "tracked.txt", "deleted.txt"],
    )?;
    git(repo.path(), &["commit", "-m", "init"])?;

    std::fs::write(repo.path().join("tracked.txt"), "current\n")?;
    std::fs::write(repo.path().join("untracked.txt"), "new\n")?;
    std::fs::write(repo.path().join("ignored.tmp"), "ignore\n")?;
    std::fs::remove_file(repo.path().join("deleted.txt"))?;

    let writes = collect_repo_worktree_snapshot(repo.path())?;
    let paths = writes
        .iter()
        .map(|item| item.path.as_str())
        .collect::<Vec<_>>();
    assert!(paths.contains(&".gitignore"));
    assert!(paths.contains(&"tracked.txt"));
    assert!(paths.contains(&"untracked.txt"));
    assert!(!paths.contains(&"ignored.tmp"));
    assert!(!paths.contains(&"deleted.txt"));
    let tracked = writes
        .iter()
        .find(|item| item.path == "tracked.txt")
        .expect("tracked write");
    let tracked_content = BASE64.decode(tracked.content_b64.as_bytes())?;
    assert_eq!(tracked_content, b"current\n");
    Ok(())
}

#[tokio::test]
async fn sync_run_round_trip_with_env_and_exec_bit() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_repo = tempfile::tempdir()?;

    std::fs::create_dir_all(local_repo.path().join("nested"))?;
    std::fs::write(
        local_repo.path().join("hello.sh"),
        "#!/usr/bin/env bash\necho \"$DEMO_FLAG\"\n",
    )?;
    std::fs::write(local_repo.path().join("nested/old.txt"), "old\n")?;
    git(local_repo.path(), &["add", "hello.sh", "nested/old.txt"])?;
    git(local_repo.path(), &["commit", "-m", "init"])?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = local_repo.path().join("hello.sh");
        let mut permissions = std::fs::metadata(&path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions)?;
    }

    std::fs::remove_file(local_repo.path().join("nested/old.txt"))?;
    std::fs::write(local_repo.path().join("nested/new.txt"), "new\n")?;

    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_repo.path().to_path_buf()],
    );
    let (writes, deletes) = collect_repo_changes(local_repo.path())?;
    let payload = SyncPayload {
        remote_root: remote_repo.path().display().to_string(),
        writes,
        deletes,
        sync_mode: SyncMode::WorktreeDelta,
        source_ref: None,
        preserve_paths: Vec::new(),
        command: Some(
            "test -x hello.sh && ./hello.sh && test ! -e nested/old.txt && cat nested/new.txt"
                .to_string(),
        ),
        timeout_seconds: 30,
        env: Some(std::iter::once(("DEMO_FLAG".to_string(), "from-env".to_string())).collect()),
        claims: Vec::new(),
        checksum: false,
        preserve_mode: false,
        atomic_files: false,
        sync_session_id: None,
        lock_token: None,
    };

    let response = handle_sync_run(&state, payload).await?;
    assert!(response.ok);
    assert!(response.result.stdout.contains("from-env"));
    assert!(response.result.stdout.contains("new"));
    assert!(!remote_repo.path().join("nested/old.txt").exists());
    Ok(())
}

#[tokio::test]
async fn sync_run_rejects_escape_and_allow_list_violation() -> Result<()> {
    let remote_repo = tempfile::tempdir()?;
    let other_repo = tempfile::tempdir()?;
    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_repo.path().to_path_buf()],
    );

    let payload = SyncPayload {
        remote_root: remote_repo.path().display().to_string(),
        writes: vec![agentplane::protocol::FileWrite {
            path: "../escape.txt".to_string(),
            content_b64: "eA==".to_string(),
            executable: false,
            mode: None,
            checksum_sha256: None,
            preuploaded: false,
            preupload_existed: false,
            preupload_skipped: false,
        }],
        deletes: vec![],
        sync_mode: SyncMode::WorktreeDelta,
        source_ref: None,
        preserve_paths: Vec::new(),
        command: None,
        timeout_seconds: 30,
        env: Some(Default::default()),
        claims: Vec::new(),
        checksum: false,
        preserve_mode: false,
        atomic_files: false,
        sync_session_id: None,
        lock_token: None,
    };
    assert!(handle_sync_run(&state, payload).await.is_err());

    let payload = SyncPayload {
        remote_root: other_repo.path().display().to_string(),
        writes: vec![],
        deletes: vec![],
        sync_mode: SyncMode::WorktreeDelta,
        source_ref: None,
        preserve_paths: Vec::new(),
        command: None,
        timeout_seconds: 30,
        env: Some(Default::default()),
        claims: Vec::new(),
        checksum: false,
        preserve_mode: false,
        atomic_files: false,
        sync_session_id: None,
        lock_token: None,
    };
    assert!(handle_sync_run(&state, payload).await.is_err());
    Ok(())
}

#[tokio::test]
async fn sync_run_allows_remote_root_equal_to_allow_root_even_if_missing_before_sync() -> Result<()>
{
    let local_repo = init_repo()?;
    let remote_parent = tempfile::tempdir()?;
    let remote_root = remote_parent.path().join("mirror");

    std::fs::write(local_repo.path().join("main.txt"), "hello\n")?;
    git(local_repo.path(), &["add", "main.txt"])?;
    git(local_repo.path(), &["commit", "-m", "init"])?;
    std::fs::write(local_repo.path().join("main.txt"), "updated\n")?;

    let state = ServerState::new("test-token".to_string(), vec![remote_root.clone()]);
    let (writes, deletes) = collect_repo_changes(local_repo.path())?;
    let payload = SyncPayload {
        remote_root: remote_root.display().to_string(),
        writes,
        deletes,
        sync_mode: SyncMode::WorktreeDelta,
        source_ref: None,
        preserve_paths: Vec::new(),
        command: Some("cat main.txt".to_string()),
        timeout_seconds: 30,
        env: Some(Default::default()),
        claims: Vec::new(),
        checksum: false,
        preserve_mode: false,
        atomic_files: false,
        sync_session_id: None,
        lock_token: None,
    };

    let response = handle_sync_run(&state, payload).await?;
    assert!(response.ok);
    assert_eq!(
        std::fs::read_to_string(remote_root.join("main.txt"))?,
        "updated\n"
    );
    Ok(())
}

#[tokio::test]
async fn sync_run_limits_large_command_output() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );

    let payload = SyncPayload {
        remote_root: remote_root.path().display().to_string(),
        writes: vec![],
        deletes: vec![],
        sync_mode: SyncMode::WorktreeDelta,
        source_ref: None,
        preserve_paths: Vec::new(),
        command: Some(
            "python3 -c \"import sys; sys.stdout.write('A' * (5 * 1024 * 1024))\"".to_string(),
        ),
        timeout_seconds: 30,
        env: Some(Default::default()),
        claims: Vec::new(),
        checksum: false,
        preserve_mode: false,
        atomic_files: false,
        sync_session_id: None,
        lock_token: None,
    };

    let response = handle_sync_run(&state, payload).await?;
    assert!(response.ok);
    assert!(
        response
            .result
            .stdout
            .contains("[agentplane] stdout truncated")
    );
    assert!(response.result.stdout.len() < 4_300_000);
    Ok(())
}

#[test]
fn collect_repo_snapshot_uses_requested_ref_even_when_worktree_is_dirty() -> Result<()> {
    let repo = init_repo()?;
    std::fs::create_dir_all(repo.path().join("nested"))?;
    std::fs::write(repo.path().join("tracked.txt"), "commit\n")?;
    std::fs::write(
        repo.path().join("nested/tool.sh"),
        "#!/usr/bin/env bash\necho committed\n",
    )?;
    git(repo.path(), &["add", "tracked.txt", "nested/tool.sh"])?;
    git(repo.path(), &["commit", "-m", "base"])?;
    git(repo.path(), &["branch", "stable"])?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = repo.path().join("nested/tool.sh");
        let mut permissions = std::fs::metadata(&path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions)?;
        git(repo.path(), &["add", "nested/tool.sh"])?;
        git(repo.path(), &["commit", "--amend", "--no-edit"])?;
        git(repo.path(), &["branch", "-f", "stable", "HEAD"])?;
    }

    std::fs::write(repo.path().join("tracked.txt"), "dirty\n")?;
    std::fs::write(repo.path().join("untracked.txt"), "scratch\n")?;
    std::fs::write(
        repo.path().join("nested/tool.sh"),
        "#!/usr/bin/env bash\necho dirty\n",
    )?;

    let resolved = resolve_ref(repo.path(), "stable")?;
    let writes = collect_repo_snapshot(repo.path(), &resolved)?;
    let tracked = writes
        .iter()
        .find(|item| item.path == "tracked.txt")
        .expect("tracked file");
    assert_eq!(decode_b64(&tracked.content_b64)?, "commit\n");
    let tool = writes
        .iter()
        .find(|item| item.path == "nested/tool.sh")
        .expect("tool file");
    assert_eq!(
        decode_b64(&tool.content_b64)?,
        "#!/usr/bin/env bash\necho committed\n"
    );
    #[cfg(unix)]
    assert!(tool.executable);
    assert!(!writes.iter().any(|item| item.path == "untracked.txt"));
    Ok(())
}

#[tokio::test]
async fn sync_run_ref_snapshot_exactly_replaces_remote_tree_and_preserves_whitelist() -> Result<()>
{
    let remote_root = tempfile::tempdir()?;
    std::fs::create_dir_all(remote_root.path().join("src"))?;
    std::fs::create_dir_all(remote_root.path().join("target/debug"))?;
    std::fs::write(remote_root.path().join("src/stale.rs"), "stale\n")?;
    std::fs::write(remote_root.path().join("remove.txt"), "remove\n")?;
    std::fs::write(remote_root.path().join("target/debug/cache.bin"), "keep\n")?;

    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );
    let payload = SyncPayload {
        remote_root: remote_root.path().display().to_string(),
        writes: vec![
            agentplane::protocol::FileWrite {
                path: "src/main.rs".to_string(),
                content_b64: BASE64.encode("fn main() {}\n"),
                executable: false,
                mode: None,
                checksum_sha256: None,
                preuploaded: false,
                preupload_existed: false,
                preupload_skipped: false,
            },
            agentplane::protocol::FileWrite {
                path: "README.md".to_string(),
                content_b64: BASE64.encode("hello\n"),
                executable: false,
                mode: None,
                checksum_sha256: None,
                preuploaded: false,
                preupload_existed: false,
                preupload_skipped: false,
            },
        ],
        deletes: vec![],
        sync_mode: SyncMode::RefSnapshot,
        source_ref: Some("deadbeef".to_string()),
        preserve_paths: vec!["target".to_string()],
        command: Some("test -f src/main.rs && test ! -e src/stale.rs && test -f target/debug/cache.bin && cat README.md".to_string()),
        timeout_seconds: 30,
        env: Some(Default::default()),
        claims: Vec::new(),
        checksum: false,
        preserve_mode: false,
        atomic_files: false,
        sync_session_id: None,
        lock_token: None,
    };

    let response = handle_sync_run(&state, payload).await?;
    assert!(response.ok);
    assert_eq!(response.delete_count, 2);
    assert_eq!(response.source_ref.as_deref(), Some("deadbeef"));
    assert_eq!(response.preserve_paths, vec!["target".to_string()]);
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("src/main.rs"))?,
        "fn main() {}\n"
    );
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("README.md"))?,
        "hello\n"
    );
    assert!(!remote_root.path().join("src/stale.rs").exists());
    assert!(!remote_root.path().join("remove.txt").exists());
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("target/debug/cache.bin"))?,
        "keep\n"
    );
    Ok(())
}

#[tokio::test]
async fn sync_run_ref_snapshot_preserve_path_does_not_block_sibling_deletes() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    std::fs::create_dir_all(remote_root.path().join("target/cache"))?;
    std::fs::create_dir_all(remote_root.path().join("src"))?;
    std::fs::write(remote_root.path().join("target/cache/data.bin"), "keep\n")?;
    std::fs::write(remote_root.path().join("src/old.rs"), "remove\n")?;

    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );
    let payload = SyncPayload {
        remote_root: remote_root.path().display().to_string(),
        writes: vec![],
        deletes: vec![],
        sync_mode: SyncMode::RefSnapshot,
        source_ref: Some("cafebabe".to_string()),
        preserve_paths: vec!["target".to_string()],
        command: None,
        timeout_seconds: 30,
        env: Some(Default::default()),
        claims: Vec::new(),
        checksum: false,
        preserve_mode: false,
        atomic_files: false,
        sync_session_id: None,
        lock_token: None,
    };

    let response = handle_sync_run(&state, payload).await?;
    assert!(response.ok);
    assert_eq!(response.delete_count, 1);
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("target/cache/data.bin"))?,
        "keep\n"
    );
    assert!(!remote_root.path().join("src/old.rs").exists());
    assert!(!remote_root.path().join("src").exists());
    Ok(())
}

#[tokio::test]
async fn sync_run_checksum_skips_identical_files_and_reports_write_categories() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    std::fs::write(remote_root.path().join("same.txt"), "same\n")?;
    std::fs::write(remote_root.path().join("changed.txt"), "old\n")?;
    std::fs::write(remote_root.path().join("delete.txt"), "delete\n")?;

    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );
    let payload = SyncPayload {
        remote_root: remote_root.path().display().to_string(),
        writes: vec![
            agentplane::protocol::FileWrite {
                path: "same.txt".to_string(),
                content_b64: BASE64.encode("same\n"),
                executable: false,
                mode: Some(0o644),
                checksum_sha256: Some(test_sha256_hex(b"same\n")),
                preuploaded: false,
                preupload_existed: false,
                preupload_skipped: false,
            },
            agentplane::protocol::FileWrite {
                path: "changed.txt".to_string(),
                content_b64: BASE64.encode("new\n"),
                executable: false,
                mode: Some(0o644),
                checksum_sha256: Some(test_sha256_hex(b"new\n")),
                preuploaded: false,
                preupload_existed: false,
                preupload_skipped: false,
            },
            agentplane::protocol::FileWrite {
                path: "created.txt".to_string(),
                content_b64: BASE64.encode("created\n"),
                executable: false,
                mode: Some(0o644),
                checksum_sha256: Some(test_sha256_hex(b"created\n")),
                preuploaded: false,
                preupload_existed: false,
                preupload_skipped: false,
            },
        ],
        deletes: vec!["delete.txt".to_string()],
        sync_mode: SyncMode::WorktreeDelta,
        source_ref: None,
        preserve_paths: Vec::new(),
        command: None,
        timeout_seconds: 30,
        env: Some(Default::default()),
        claims: Vec::new(),
        checksum: true,
        preserve_mode: true,
        atomic_files: true,
        sync_session_id: None,
        lock_token: None,
    };

    let response = handle_sync_run(&state, payload).await?;
    assert!(response.ok);
    assert_eq!(response.write_count, 2);
    assert_eq!(response.delete_count, 1);
    assert_eq!(response.report.skipped, vec!["same.txt".to_string()]);
    assert_eq!(response.report.updated, vec!["changed.txt".to_string()]);
    assert_eq!(response.report.created, vec!["created.txt".to_string()]);
    assert_eq!(response.report.deleted, vec!["delete.txt".to_string()]);
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("changed.txt"))?,
        "new\n"
    );
    assert!(!remote_root.path().join("delete.txt").exists());
    Ok(())
}

#[test]
fn cli_health_and_sync_run_round_trip() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root_parent = tempfile::tempdir()?;
    let remote_root = remote_root_parent.path().join("mirror");
    let token = "test-token";

    std::fs::create_dir_all(local_repo.path().join("nested"))?;
    std::fs::write(
        local_repo.path().join("hello.sh"),
        "#!/usr/bin/env bash\necho \"$DEMO_FLAG\"\n",
    )?;
    std::fs::write(local_repo.path().join("nested/old.txt"), "old\n")?;
    git(local_repo.path(), &["add", "hello.sh", "nested/old.txt"])?;
    git(local_repo.path(), &["commit", "-m", "init"])?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = local_repo.path().join("hello.sh");
        let mut permissions = std::fs::metadata(&path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions)?;
    }

    std::fs::remove_file(local_repo.path().join("nested/old.txt"))?;
    std::fs::write(local_repo.path().join("nested/new.txt"), "new\n")?;

    let harness = CliServerHarness::start(&remote_root, token)?;

    let health = run_cli(&["health", "--server", &harness.base_url])?;
    assert!(health.status.success());
    let health_body: serde_json::Value = serde_json::from_slice(&health.stdout)?;
    assert_eq!(health_body["ok"], true);

    let sync = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.display().to_string(),
        "--env",
        "DEMO_FLAG=from-cli",
        "--command",
        "test -x hello.sh && ./hello.sh && test ! -e nested/old.txt && cat nested/new.txt",
    ])?;
    assert!(sync.status.success());
    let sync_body: serde_json::Value = serde_json::from_slice(&sync.stdout)?;
    assert_eq!(sync_body["ok"], true);
    assert_eq!(
        std::fs::read_to_string(remote_root.join("nested/new.txt"))?,
        "new\n"
    );
    let stdout = sync_body["result"]["stdout"].as_str().unwrap_or_default();
    assert!(stdout.contains("from-cli"));
    assert!(stdout.contains("new"));
    Ok(())
}

#[test]
fn cli_sync_run_no_changes_still_executes_command() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";

    std::fs::write(local_repo.path().join("main.txt"), "same\n")?;
    git(local_repo.path(), &["add", "main.txt"])?;
    git(local_repo.path(), &["commit", "-m", "init"])?;
    std::fs::write(remote_root.path().join("main.txt"), "same\n")?;

    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let sync = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--command",
        "cat main.txt",
    ])?;
    assert!(sync.status.success());
    let sync_body: serde_json::Value = serde_json::from_slice(&sync.stdout)?;
    assert_eq!(sync_body["write_count"], 0);
    assert_eq!(sync_body["delete_count"], 0);
    assert!(
        sync_body["result"]["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("same")
    );
    Ok(())
}

#[test]
fn cli_sync_run_transfers_large_file_with_upload_chunks() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let content = (0..(2 * 1024 * 1024))
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    std::fs::write(local_repo.path().join("large.bin"), &content)?;

    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let sync = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--upload-chunk-size",
        "4096",
        "--checksum",
        "--command",
        "test -s large.bin",
    ])?;
    assert!(
        sync.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&sync.stderr)
    );
    let sync_body: serde_json::Value = serde_json::from_slice(&sync.stdout)?;
    assert_eq!(sync_body["ok"], true);
    assert_eq!(sync_body["write_count"], 1);
    assert_eq!(
        std::fs::read(remote_root.path().join("large.bin"))?,
        content
    );
    Ok(())
}

#[test]
fn cli_sync_init_initializes_remote_directory_from_current_git_project() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";

    std::fs::write(local_repo.path().join(".gitignore"), "ignored.tmp\n")?;
    std::fs::write(local_repo.path().join("tracked.txt"), "old\n")?;
    git(local_repo.path(), &["add", ".gitignore", "tracked.txt"])?;
    git(local_repo.path(), &["commit", "-m", "init"])?;
    std::fs::write(local_repo.path().join("tracked.txt"), "current\n")?;
    std::fs::write(local_repo.path().join("untracked.txt"), "new\n")?;
    std::fs::write(local_repo.path().join("ignored.tmp"), "ignore\n")?;
    std::fs::write(remote_root.path().join("stale.txt"), "stale\n")?;

    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let sync = run_cli(&[
        "sync-init",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--upload-chunk-size",
        "4096",
        "--checksum",
    ])?;
    assert!(
        sync.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&sync.stderr)
    );
    let sync_body: serde_json::Value = serde_json::from_slice(&sync.stdout)?;
    assert_eq!(sync_body["ok"], true);
    assert_eq!(sync_body["source_ref"], "worktree");
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("tracked.txt"))?,
        "current\n"
    );
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("untracked.txt"))?,
        "new\n"
    );
    assert!(!remote_root.path().join("ignored.tmp").exists());
    assert!(!remote_root.path().join("stale.txt").exists());
    Ok(())
}

#[test]
fn cli_sync_run_reports_failure_timeout_and_auth_error() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";

    std::fs::write(local_repo.path().join("main.txt"), "hello\n")?;
    git(local_repo.path(), &["add", "main.txt"])?;
    git(local_repo.path(), &["commit", "-m", "init"])?;
    std::fs::write(local_repo.path().join("main.txt"), "updated\n")?;

    let harness = CliServerHarness::start(remote_root.path(), token)?;

    let failed = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--command",
        "echo boom >&2; exit 7",
    ])?;
    assert!(!failed.status.success());
    let failed_body: serde_json::Value = serde_json::from_slice(&failed.stdout)?;
    assert_eq!(failed_body["ok"], false);
    assert_eq!(failed_body["result"]["exit_code"], 7);

    let timed_out = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--timeout-seconds",
        "1",
        "--command",
        "sleep 2",
    ])?;
    assert!(!timed_out.status.success());
    let timeout_stderr = String::from_utf8(timed_out.stderr)?;
    assert!(timeout_stderr.contains("timed out"));

    let unauthorized = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        "wrong-token",
        "--remote-root",
        &remote_root.path().display().to_string(),
    ])?;
    assert!(!unauthorized.status.success());
    let unauthorized_stderr = String::from_utf8(unauthorized.stderr)?;
    assert!(unauthorized_stderr.contains("unauthorized"));
    Ok(())
}

#[test]
fn cli_sync_run_ref_exact_mirror_and_preserve_path() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";

    std::fs::create_dir_all(local_repo.path().join("src"))?;
    std::fs::write(
        local_repo.path().join("src/main.rs"),
        "fn main() { println!(\"v1\"); }\n",
    )?;
    std::fs::write(local_repo.path().join("old.txt"), "old\n")?;
    git(local_repo.path(), &["add", "."])?;
    git(local_repo.path(), &["commit", "-m", "v1"])?;

    std::fs::remove_file(local_repo.path().join("old.txt"))?;
    std::fs::create_dir_all(local_repo.path().join("bin"))?;
    std::fs::write(
        local_repo.path().join("bin/run.sh"),
        "#!/usr/bin/env bash\necho v2\n",
    )?;
    std::fs::write(local_repo.path().join("README.md"), "hello\n")?;
    git(local_repo.path(), &["add", "-A"])?;
    git(local_repo.path(), &["commit", "-m", "v2"])?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = local_repo.path().join("bin/run.sh");
        let mut permissions = std::fs::metadata(&path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions)?;
        git(local_repo.path(), &["add", "bin/run.sh"])?;
        git(local_repo.path(), &["commit", "--amend", "--no-edit"])?;
    }
    let head = git_output(local_repo.path(), &["rev-parse", "HEAD"])?;

    std::fs::write(remote_root.path().join("extra.txt"), "stale\n")?;
    std::fs::create_dir_all(remote_root.path().join("target/debug"))?;
    std::fs::write(remote_root.path().join("target/debug/cache.bin"), "keep\n")?;

    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let sync = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        &head,
        "--preserve-path",
        "target",
        "--command",
        "test -x bin/run.sh && ./bin/run.sh && test ! -e extra.txt && cat README.md",
    ])?;
    assert!(
        sync.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&sync.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&sync.stdout)?;
    assert_eq!(body["ok"], true);
    assert_eq!(body["source_ref"].as_str().unwrap_or_default(), head);
    assert!(body["delete_count"].as_u64().unwrap_or_default() >= 1);
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("README.md"))?,
        "hello\n"
    );
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("target/debug/cache.bin"))?,
        "keep\n"
    );
    assert!(!remote_root.path().join("extra.txt").exists());
    Ok(())
}

#[test]
fn cli_sync_run_ref_dry_run_reports_delete_plan() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";

    std::fs::write(local_repo.path().join("main.txt"), "tracked\n")?;
    git(local_repo.path(), &["add", "main.txt"])?;
    git(local_repo.path(), &["commit", "-m", "init"])?;

    std::fs::write(remote_root.path().join("main.txt"), "old\n")?;
    std::fs::write(remote_root.path().join("remove.txt"), "remove\n")?;
    std::fs::create_dir_all(remote_root.path().join("target"))?;
    std::fs::write(remote_root.path().join("target/cache.bin"), "keep\n")?;

    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let output = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        "HEAD",
        "--preserve-path",
        "target",
        "--dry-run",
    ])?;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(body["sync_mode"], "ref_snapshot");
    assert_eq!(body["source_ref"].as_str().unwrap_or_default().len(), 40);
    assert_eq!(body["delete_count"], 1);
    let deletes = body["deletes"].as_array().cloned().unwrap_or_default();
    assert_eq!(deletes.len(), 1);
    assert_eq!(deletes[0], "remove.txt");
    Ok(())
}

#[test]
fn cli_sync_run_dry_run_filters_delta_with_include_and_exclude_from() -> Result<()> {
    let local_repo = init_repo()?;
    std::fs::create_dir_all(local_repo.path().join("src"))?;
    std::fs::write(local_repo.path().join("src/keep.rs"), "old\n")?;
    std::fs::write(local_repo.path().join("src/drop.rs"), "old\n")?;
    std::fs::write(local_repo.path().join("README.md"), "old\n")?;
    git(local_repo.path(), &["add", "."])?;
    git(local_repo.path(), &["commit", "-m", "init"])?;

    std::fs::write(local_repo.path().join("src/keep.rs"), "new\n")?;
    std::fs::write(local_repo.path().join("src/drop.rs"), "new\n")?;
    std::fs::write(local_repo.path().join("README.md"), "new\n")?;
    let exclude_file = local_repo.path().join("cc-exclude.txt");
    std::fs::write(&exclude_file, "src/drop.rs\n")?;

    let output = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        "http://127.0.0.1:1",
        "--token",
        "unused",
        "--remote-root",
        "/tmp/unused",
        "--include",
        "src/*",
        "--exclude-from",
        &exclude_file.display().to_string(),
        "--dry-run",
    ])?;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let writes = body["writes"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        writes,
        vec![serde_json::Value::String("src/keep.rs".to_string())]
    );
    Ok(())
}

#[test]
fn cli_sync_run_ref_rejects_invalid_ref_without_touching_remote() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";

    std::fs::write(local_repo.path().join("main.txt"), "tracked\n")?;
    git(local_repo.path(), &["add", "main.txt"])?;
    git(local_repo.path(), &["commit", "-m", "init"])?;
    std::fs::write(remote_root.path().join("sentinel.txt"), "stay\n")?;

    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let output = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        "missing-ref-name",
    ])?;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("git rev-parse missing-ref-name failed"));
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("sentinel.txt"))?,
        "stay\n"
    );
    Ok(())
}

#[test]
fn cli_sync_run_ref_branch_ignores_dirty_checkout_and_preserves_multiple_paths() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";

    std::fs::create_dir_all(local_repo.path().join("src"))?;
    std::fs::write(
        local_repo.path().join("src/main.rs"),
        "fn main() { println!(\"stable\"); }\n",
    )?;
    std::fs::write(
        local_repo.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )?;
    git(local_repo.path(), &["add", "."])?;
    git(local_repo.path(), &["commit", "-m", "stable-base"])?;
    git(local_repo.path(), &["branch", "stable"])?;

    std::fs::write(
        local_repo.path().join("src/main.rs"),
        "fn main() { println!(\"feature\"); }\n",
    )?;
    std::fs::write(
        local_repo.path().join("feature.txt"),
        "feature branch only\n",
    )?;
    git(local_repo.path(), &["add", "."])?;
    git(local_repo.path(), &["commit", "-m", "feature-work"])?;

    std::fs::write(
        local_repo.path().join("src/main.rs"),
        "fn main() { println!(\"dirty\"); }\n",
    )?;
    std::fs::write(local_repo.path().join("scratch.tmp"), "dirty scratch\n")?;

    std::fs::create_dir_all(remote_root.path().join("target/debug"))?;
    std::fs::create_dir_all(remote_root.path().join("models"))?;
    std::fs::create_dir_all(remote_root.path().join("src"))?;
    std::fs::write(
        remote_root.path().join("target/debug/cache.bin"),
        "keep-target\n",
    )?;
    std::fs::write(remote_root.path().join("models/model.bin"), "keep-model\n")?;
    std::fs::write(
        remote_root.path().join("feature.txt"),
        "stale remote feature\n",
    )?;
    std::fs::write(remote_root.path().join("src/extra.rs"), "remove me\n")?;

    let harness = CliServerHarness::start(remote_root.path(), token)?;

    let dry_run = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        "stable",
        "--preserve-path",
        "target",
        "--preserve-path",
        "models",
        "--dry-run",
    ])?;
    assert!(
        dry_run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let dry_run_body: serde_json::Value = serde_json::from_slice(&dry_run.stdout)?;
    assert_eq!(dry_run_body["sync_mode"], "ref_snapshot");
    assert_eq!(dry_run_body["write_count"], 2);
    let deletes = dry_run_body["deletes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(deletes.iter().any(|value| value == "feature.txt"));
    assert!(deletes.iter().any(|value| value == "src/extra.rs"));
    assert!(
        !deletes
            .iter()
            .any(|value| value == "target/debug/cache.bin")
    );
    assert!(!deletes.iter().any(|value| value == "models/model.bin"));

    let sync = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        "stable",
        "--preserve-path",
        "target",
        "--preserve-path",
        "models",
        "--command",
        "test ! -e feature.txt && test ! -e src/extra.rs && test -f target/debug/cache.bin && test -f models/model.bin && cat src/main.rs",
    ])?;
    assert!(
        sync.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&sync.stderr)
    );
    let sync_body: serde_json::Value = serde_json::from_slice(&sync.stdout)?;
    assert_eq!(sync_body["ok"], true);
    let stdout = sync_body["result"]["stdout"].as_str().unwrap_or_default();
    assert!(stdout.contains("stable"));
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("src/main.rs"))?,
        "fn main() { println!(\"stable\"); }\n"
    );
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("target/debug/cache.bin"))?,
        "keep-target\n"
    );
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("models/model.bin"))?,
        "keep-model\n"
    );
    assert!(!remote_root.path().join("feature.txt").exists());
    assert!(!remote_root.path().join("src/extra.rs").exists());
    assert!(!remote_root.path().join("scratch.tmp").exists());
    Ok(())
}

#[test]
fn cli_sync_run_ref_can_switch_remote_between_two_refs_and_then_run_process() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";

    std::fs::create_dir_all(local_repo.path().join("src"))?;
    std::fs::write(local_repo.path().join("src/app.txt"), "version-one\n")?;
    std::fs::write(local_repo.path().join("old.txt"), "old\n")?;
    git(local_repo.path(), &["add", "."])?;
    git(local_repo.path(), &["commit", "-m", "v1"])?;
    let first_ref = git_output(local_repo.path(), &["rev-parse", "HEAD"])?;

    std::fs::remove_file(local_repo.path().join("old.txt"))?;
    std::fs::create_dir_all(local_repo.path().join("bin"))?;
    std::fs::write(
        local_repo.path().join("bin/info.sh"),
        "#!/usr/bin/env bash\necho version-two\n",
    )?;
    std::fs::write(local_repo.path().join("src/app.txt"), "version-two\n")?;
    git(local_repo.path(), &["add", "-A"])?;
    git(local_repo.path(), &["commit", "-m", "v2"])?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = local_repo.path().join("bin/info.sh");
        let mut permissions = std::fs::metadata(&path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions)?;
        git(local_repo.path(), &["add", "bin/info.sh"])?;
        git(local_repo.path(), &["commit", "--amend", "--no-edit"])?;
    }
    let second_ref = git_output(local_repo.path(), &["rev-parse", "HEAD"])?;

    std::fs::create_dir_all(remote_root.path().join("target/tmp"))?;
    std::fs::write(
        remote_root.path().join("target/tmp/cache.bin"),
        "cache-stays\n",
    )?;
    let harness = CliServerHarness::start(remote_root.path(), token)?;

    let sync_first = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        &first_ref,
        "--preserve-path",
        "target",
        "--command",
        "test -f old.txt && test ! -e bin/info.sh && cat src/app.txt",
    ])?;
    assert!(
        sync_first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&sync_first.stderr)
    );
    let first_body: serde_json::Value = serde_json::from_slice(&sync_first.stdout)?;
    assert!(
        first_body["result"]["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("version-one")
    );
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("src/app.txt"))?,
        "version-one\n"
    );
    assert!(remote_root.path().join("old.txt").exists());
    assert!(!remote_root.path().join("bin/info.sh").exists());

    let sync_second = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        &second_ref,
        "--preserve-path",
        "target",
        "--command",
        "test ! -e old.txt && test -x bin/info.sh && ./bin/info.sh && cat src/app.txt",
    ])?;
    assert!(
        sync_second.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&sync_second.stderr)
    );
    let second_body: serde_json::Value = serde_json::from_slice(&sync_second.stdout)?;
    let second_stdout = second_body["result"]["stdout"].as_str().unwrap_or_default();
    assert!(second_stdout.contains("version-two"));
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("src/app.txt"))?,
        "version-two\n"
    );
    assert!(!remote_root.path().join("old.txt").exists());
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("target/tmp/cache.bin"))?,
        "cache-stays\n"
    );

    let process_start = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "switch-check",
        "--",
        "bash",
        "-lc",
        "cat src/app.txt; ./bin/info.sh; cat target/tmp/cache.bin",
    ])?;
    assert!(
        process_start.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&process_start.stderr)
    );

    let process_read = run_cli(&[
        "process-read",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "switch-check",
        "--follow",
        "--text",
    ])?;
    assert!(
        process_read.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&process_read.stderr)
    );
    let process_stdout = String::from_utf8(process_read.stdout)?;
    assert!(process_stdout.contains("version-two"));
    assert!(process_stdout.contains("cache-stays"));
    Ok(())
}

#[test]
fn cli_sync_run_ref_dry_run_succeeds_when_remote_root_is_missing() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root_parent = tempfile::tempdir()?;
    let remote_root = remote_root_parent.path().join("missing-remote-root");
    let token = "test-token";

    std::fs::write(local_repo.path().join("main.txt"), "tracked\n")?;
    git(local_repo.path(), &["add", "main.txt"])?;
    git(local_repo.path(), &["commit", "-m", "init"])?;

    let harness = CliServerHarness::start(&remote_root, token)?;
    let output = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.display().to_string(),
        "--ref",
        "HEAD",
        "--dry-run",
    ])?;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(body["delete_count"], 0);
    let deletes = body["deletes"].as_array().cloned().unwrap_or_default();
    assert!(deletes.is_empty());
    assert!(!remote_root.exists());
    Ok(())
}

#[test]
fn cli_sync_run_ref_dry_run_does_not_mutate_remote_state() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";

    std::fs::write(local_repo.path().join("main.txt"), "tracked\n")?;
    git(local_repo.path(), &["add", "main.txt"])?;
    git(local_repo.path(), &["commit", "-m", "init"])?;

    std::fs::create_dir_all(remote_root.path().join("src"))?;
    std::fs::write(remote_root.path().join("src/stale.rs"), "stale\n")?;
    std::fs::write(remote_root.path().join("remove.txt"), "remove\n")?;

    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let output = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        "HEAD",
        "--dry-run",
    ])?;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("src/stale.rs"))?,
        "stale\n"
    );
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("remove.txt"))?,
        "remove\n"
    );
    Ok(())
}

#[test]
fn cli_sync_run_ref_preserve_prefix_match_does_not_overpreserve_similar_names() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";

    std::fs::write(local_repo.path().join("main.txt"), "tracked\n")?;
    git(local_repo.path(), &["add", "main.txt"])?;
    git(local_repo.path(), &["commit", "-m", "init"])?;

    std::fs::create_dir_all(remote_root.path().join("target/debug"))?;
    std::fs::create_dir_all(remote_root.path().join("target-cache"))?;
    std::fs::write(remote_root.path().join("target/debug/cache.bin"), "keep\n")?;
    std::fs::write(
        remote_root.path().join("target-cache/stale.bin"),
        "remove\n",
    )?;

    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let sync = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        "HEAD",
        "--preserve-path",
        "target",
        "--command",
        "test -f target/debug/cache.bin && test ! -e target-cache/stale.bin && cat main.txt",
    ])?;
    assert!(
        sync.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&sync.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("target/debug/cache.bin"))?,
        "keep\n"
    );
    assert!(!remote_root.path().join("target-cache/stale.bin").exists());
    Ok(())
}

#[test]
fn cli_sync_run_ref_empty_tree_clears_remote_except_preserved_paths() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";

    std::fs::write(local_repo.path().join("main.txt"), "tracked\n")?;
    git(local_repo.path(), &["add", "main.txt"])?;
    git(local_repo.path(), &["commit", "-m", "seed"])?;
    git(local_repo.path(), &["rm", "main.txt"])?;
    git(local_repo.path(), &["commit", "-m", "empty-tree"])?;

    std::fs::create_dir_all(remote_root.path().join("src"))?;
    std::fs::create_dir_all(remote_root.path().join("models"))?;
    std::fs::write(remote_root.path().join("src/app.rs"), "remove\n")?;
    std::fs::write(remote_root.path().join("models/model.bin"), "keep\n")?;

    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let sync = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        "HEAD",
        "--preserve-path",
        "models",
        "--command",
        "test ! -e src/app.rs && test -f models/model.bin",
    ])?;
    assert!(
        sync.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&sync.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&sync.stdout)?;
    assert_eq!(body["write_count"], 0);
    assert!(body["delete_count"].as_u64().unwrap_or_default() >= 1);
    assert!(!remote_root.path().join("src/app.rs").exists());
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("models/model.bin"))?,
        "keep\n"
    );
    Ok(())
}

#[test]
fn cli_sync_run_ref_creates_missing_remote_root_on_real_sync() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root_parent = tempfile::tempdir()?;
    let remote_root = remote_root_parent.path().join("created-on-sync");
    let token = "test-token";

    std::fs::create_dir_all(local_repo.path().join("src"))?;
    std::fs::write(local_repo.path().join("src/main.rs"), "fn main() {}\n")?;
    git(local_repo.path(), &["add", "."])?;
    git(local_repo.path(), &["commit", "-m", "init"])?;

    let harness = CliServerHarness::start(&remote_root, token)?;
    let sync = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.display().to_string(),
        "--ref",
        "HEAD",
        "--command",
        "test -f src/main.rs && cat src/main.rs",
    ])?;
    assert!(
        sync.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&sync.stderr)
    );
    assert!(remote_root.exists());
    assert_eq!(
        std::fs::read_to_string(remote_root.join("src/main.rs"))?,
        "fn main() {}\n"
    );
    Ok(())
}

#[test]
fn cli_sync_run_ref_updates_executable_bit_when_switching_refs() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";

    std::fs::create_dir_all(local_repo.path().join("bin"))?;
    std::fs::write(
        local_repo.path().join("bin/tool.sh"),
        "#!/usr/bin/env bash\necho one\n",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = local_repo.path().join("bin/tool.sh");
        let mut permissions = std::fs::metadata(&path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions)?;
    }
    git(local_repo.path(), &["add", "."])?;
    git(local_repo.path(), &["commit", "-m", "exec-on"])?;
    let exec_ref = git_output(local_repo.path(), &["rev-parse", "HEAD"])?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = local_repo.path().join("bin/tool.sh");
        let mut permissions = std::fs::metadata(&path)?.permissions();
        permissions.set_mode(0o644);
        std::fs::set_permissions(&path, permissions)?;
    }
    git(local_repo.path(), &["add", "bin/tool.sh"])?;
    git(local_repo.path(), &["commit", "-m", "exec-off"])?;
    let non_exec_ref = git_output(local_repo.path(), &["rev-parse", "HEAD"])?;

    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let first_sync = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        &exec_ref,
        "--command",
        "test -x bin/tool.sh && ./bin/tool.sh",
    ])?;
    assert!(
        first_sync.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first_sync.stderr)
    );

    let second_sync = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        &non_exec_ref,
        "--command",
        "test ! -x bin/tool.sh && cat bin/tool.sh",
    ])?;
    assert!(
        second_sync.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&second_sync.stderr)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(remote_root.path().join("bin/tool.sh"))?
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0);
    }
    Ok(())
}

#[test]
fn cli_sync_run_base_ref_sends_only_committed_delta_instead_of_full_snapshot() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";

    std::fs::create_dir_all(local_repo.path().join("src"))?;
    std::fs::write(local_repo.path().join("src/lib.rs"), "pub fn base() {}\n")?;
    std::fs::write(local_repo.path().join("mod.rs"), "mod base;\n")?;
    git(local_repo.path(), &["add", "."])?;
    git(local_repo.path(), &["commit", "-m", "base"])?;
    let base_ref = git_output(local_repo.path(), &["rev-parse", "HEAD"])?;

    std::fs::write(
        local_repo.path().join("src/completions.rs"),
        "pub fn completions() {}\n",
    )?;
    std::fs::write(
        local_repo.path().join("mod.rs"),
        "mod base;\nmod completions;\n",
    )?;
    git(local_repo.path(), &["add", "."])?;
    git(local_repo.path(), &["commit", "-m", "feature"])?;
    let target_ref = git_output(local_repo.path(), &["rev-parse", "HEAD"])?;

    std::fs::write(remote_root.path().join("mod.rs"), "mod base;\n")?;
    std::fs::create_dir_all(remote_root.path().join("src"))?;
    std::fs::write(remote_root.path().join("src/lib.rs"), "pub fn base() {}\n")?;

    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let dry_run = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        &target_ref,
        "--base-ref",
        &base_ref,
        "--dry-run",
    ])?;
    assert!(
        dry_run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let dry_run_body: serde_json::Value = serde_json::from_slice(&dry_run.stdout)?;
    assert_eq!(dry_run_body["sync_mode"], "worktree_delta");
    assert_eq!(dry_run_body["write_count"], 2);
    assert_eq!(dry_run_body["delete_count"], 0);
    let writes = dry_run_body["writes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(writes.len(), 2);
    assert!(writes.iter().any(|value| value == "mod.rs"));
    assert!(writes.iter().any(|value| value == "src/completions.rs"));
    assert!(!writes.iter().any(|value| value == "src/lib.rs"));

    let sync = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        &target_ref,
        "--base-ref",
        &base_ref,
        "--command",
        "test -f src/completions.rs && cat mod.rs && cat src/completions.rs",
    ])?;
    assert!(
        sync.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&sync.stderr)
    );
    let sync_body: serde_json::Value = serde_json::from_slice(&sync.stdout)?;
    assert_eq!(sync_body["ok"], true);
    assert_eq!(
        sync_body["source_ref"].as_str().unwrap_or_default(),
        format!("{base_ref}..{target_ref}")
    );
    let stdout = sync_body["result"]["stdout"].as_str().unwrap_or_default();
    assert!(stdout.contains("mod completions;"));
    assert!(stdout.contains("pub fn completions() {}"));
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("src/lib.rs"))?,
        "pub fn base() {}\n"
    );
    Ok(())
}

#[test]
fn cli_sync_run_base_ref_propagates_committed_deletes() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";

    std::fs::create_dir_all(local_repo.path().join("src"))?;
    std::fs::write(local_repo.path().join("src/lib.rs"), "pub fn base() {}\n")?;
    std::fs::write(
        local_repo.path().join("obsolete.rs"),
        "pub fn obsolete() {}\n",
    )?;
    std::fs::write(
        local_repo.path().join("mod.rs"),
        "mod base;\nmod obsolete;\n",
    )?;
    git(local_repo.path(), &["add", "."])?;
    git(local_repo.path(), &["commit", "-m", "base"])?;
    let base_ref = git_output(local_repo.path(), &["rev-parse", "HEAD"])?;

    git(local_repo.path(), &["rm", "obsolete.rs"])?;
    std::fs::write(local_repo.path().join("mod.rs"), "mod base;\n")?;
    std::fs::write(
        local_repo.path().join("src/completions.rs"),
        "pub fn completions() {}\n",
    )?;
    git(local_repo.path(), &["add", "mod.rs", "src/completions.rs"])?;
    git(local_repo.path(), &["commit", "-m", "feature-with-delete"])?;
    let target_ref = git_output(local_repo.path(), &["rev-parse", "HEAD"])?;

    std::fs::create_dir_all(remote_root.path().join("src"))?;
    std::fs::write(remote_root.path().join("src/lib.rs"), "pub fn base() {}\n")?;
    std::fs::write(
        remote_root.path().join("obsolete.rs"),
        "pub fn obsolete() {}\n",
    )?;
    std::fs::write(
        remote_root.path().join("mod.rs"),
        "mod base;\nmod obsolete;\n",
    )?;

    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let dry_run = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        &target_ref,
        "--base-ref",
        &base_ref,
        "--dry-run",
    ])?;
    assert!(
        dry_run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let dry_run_body: serde_json::Value = serde_json::from_slice(&dry_run.stdout)?;
    assert_eq!(dry_run_body["sync_mode"], "worktree_delta");
    assert_eq!(dry_run_body["write_count"], 2);
    assert_eq!(dry_run_body["delete_count"], 1);
    let writes = dry_run_body["writes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(writes.iter().any(|value| value == "mod.rs"));
    assert!(writes.iter().any(|value| value == "src/completions.rs"));
    let deletes = dry_run_body["deletes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(deletes.len(), 1);
    assert_eq!(deletes[0], "obsolete.rs");

    let sync = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        &target_ref,
        "--base-ref",
        &base_ref,
        "--command",
        "test ! -e obsolete.rs && test -f src/completions.rs && grep -q '^mod base;$' mod.rs",
    ])?;
    assert!(
        sync.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&sync.stderr)
    );
    let sync_body: serde_json::Value = serde_json::from_slice(&sync.stdout)?;
    assert_eq!(sync_body["ok"], true);
    assert_eq!(sync_body["write_count"], 2);
    assert_eq!(sync_body["delete_count"], 1);
    assert!(!remote_root.path().join("obsolete.rs").exists());
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("mod.rs"))?,
        "mod base;\n"
    );
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("src/completions.rs"))?,
        "pub fn completions() {}\n"
    );
    Ok(())
}

#[test]
fn cli_sync_run_ref_supports_paths_with_spaces() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";

    std::fs::create_dir_all(local_repo.path().join("dir with spaces"))?;
    std::fs::write(
        local_repo.path().join("dir with spaces/file name.txt"),
        "spaced\n",
    )?;
    git(local_repo.path(), &["add", "."])?;
    git(local_repo.path(), &["commit", "-m", "spaces"])?;

    std::fs::create_dir_all(remote_root.path().join("dir with spaces"))?;
    std::fs::write(
        remote_root.path().join("dir with spaces/old name.txt"),
        "remove\n",
    )?;

    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let sync = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        "HEAD",
        "--command",
        "test -f 'dir with spaces/file name.txt' && test ! -e 'dir with spaces/old name.txt' && cat 'dir with spaces/file name.txt'",
    ])?;
    assert!(
        sync.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&sync.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("dir with spaces/file name.txt"))?,
        "spaced\n"
    );
    assert!(
        !remote_root
            .path()
            .join("dir with spaces/old name.txt")
            .exists()
    );
    Ok(())
}

#[test]
fn cli_sync_run_ref_preserve_subtree_allows_sibling_cleanup() -> Result<()> {
    let local_repo = init_repo()?;
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";

    std::fs::create_dir_all(local_repo.path().join("src"))?;
    std::fs::write(local_repo.path().join("src/main.rs"), "fn main() {}\n")?;
    git(local_repo.path(), &["add", "."])?;
    git(local_repo.path(), &["commit", "-m", "init"])?;

    std::fs::create_dir_all(remote_root.path().join("build/cache"))?;
    std::fs::create_dir_all(remote_root.path().join("build/tmp"))?;
    std::fs::write(remote_root.path().join("build/cache/keep.bin"), "keep\n")?;
    std::fs::write(remote_root.path().join("build/tmp/remove.bin"), "remove\n")?;

    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let sync = run_cli(&[
        "sync-run",
        "--repo",
        &local_repo.path().display().to_string(),
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--ref",
        "HEAD",
        "--preserve-path",
        "build/cache",
        "--command",
        "test -f build/cache/keep.bin && test ! -e build/tmp/remove.bin && cat src/main.rs",
    ])?;
    assert!(
        sync.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&sync.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("build/cache/keep.bin"))?,
        "keep\n"
    );
    assert!(!remote_root.path().join("build/tmp/remove.bin").exists());
    Ok(())
}
