use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha2::{Digest, Sha256};

use crate::protocol::FileWrite;

pub fn resolve_repo_root(repo: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(repo)
        .output()
        .with_context(|| format!("failed to run git in {}", repo.display()))?;

    if !output.status.success() {
        bail!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let root = String::from_utf8(output.stdout)?.trim().to_string();
    Ok(PathBuf::from(root))
}

pub fn resolve_ref(repo: &Path, git_ref: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", git_ref])
        .current_dir(repo)
        .output()
        .with_context(|| format!("failed to resolve git ref {git_ref} in {}", repo.display()))?;

    if !output.status.success() {
        bail!(
            "git rev-parse {} failed: {}",
            git_ref,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn run_git_lines(repo: &Path, args: &[&str]) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .with_context(|| format!("failed to run git {:?} in {}", args, repo.display()))?;

    if !output.status.success() {
        bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let stdout = String::from_utf8(output.stdout)?;
    Ok(stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn run_git_output(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .with_context(|| format!("failed to run git {:?} in {}", args, repo.display()))?;

    if !output.status.success() {
        bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(output.stdout)
}

pub fn parse_env_pairs(values: &[String]) -> Result<BTreeMap<String, String>> {
    let mut env = BTreeMap::new();
    for value in values {
        let Some((key, item)) = value.split_once('=') else {
            bail!("expected KEY=VALUE, got: {value}");
        };
        if key.is_empty() {
            bail!("empty key in env item: {value}");
        }
        env.insert(key.to_string(), item.to_string());
    }
    Ok(env)
}

pub fn collect_repo_changes(repo: &Path) -> Result<(Vec<FileWrite>, Vec<String>)> {
    let mut changed = BTreeSet::new();
    for args in [
        ["diff", "--name-only", "--relative"].as_slice(),
        ["diff", "--cached", "--name-only", "--relative"].as_slice(),
        ["ls-files", "--others", "--exclude-standard"].as_slice(),
    ] {
        for line in run_git_lines(repo, args)? {
            changed.insert(line);
        }
    }

    let mut deleted = BTreeSet::new();
    for args in [
        ["diff", "--name-only", "--diff-filter=D", "--relative"].as_slice(),
        [
            "diff",
            "--cached",
            "--name-only",
            "--diff-filter=D",
            "--relative",
        ]
        .as_slice(),
    ] {
        for line in run_git_lines(repo, args)? {
            deleted.insert(line);
        }
    }

    let mut writes = Vec::new();
    for relative in changed.difference(&deleted) {
        let absolute = repo.join(relative);
        if !absolute.is_file() {
            continue;
        }

        let content = std::fs::read(&absolute)
            .with_context(|| format!("failed to read {}", absolute.display()))?;
        let executable = is_executable(&absolute)?;
        writes.push(FileWrite {
            path: relative.clone(),
            content_b64: BASE64.encode(&content),
            executable,
            mode: file_mode(&absolute)?,
            checksum_sha256: Some(sha256_hex(&content)),
            preuploaded: false,
            preupload_existed: false,
            preupload_skipped: false,
        });
    }

    Ok((writes, deleted.into_iter().collect()))
}

pub fn collect_repo_worktree_snapshot(repo: &Path) -> Result<Vec<FileWrite>> {
    let mut paths = BTreeSet::new();
    for line in run_git_lines(
        repo,
        &["ls-files", "--cached", "--others", "--exclude-standard"],
    )? {
        paths.insert(line);
    }

    let mut writes = Vec::new();
    for relative in paths {
        let absolute = repo.join(&relative);
        if !absolute.is_file() {
            continue;
        }

        let content = std::fs::read(&absolute)
            .with_context(|| format!("failed to read {}", absolute.display()))?;
        let executable = is_executable(&absolute)?;
        writes.push(FileWrite {
            path: relative,
            content_b64: BASE64.encode(&content),
            executable,
            mode: file_mode(&absolute)?,
            checksum_sha256: Some(sha256_hex(&content)),
            preuploaded: false,
            preupload_existed: false,
            preupload_skipped: false,
        });
    }

    Ok(writes)
}

pub fn collect_repo_snapshot(repo: &Path, resolved_ref: &str) -> Result<Vec<FileWrite>> {
    let tree_entries = run_git_lines(repo, &["ls-tree", "-r", "--full-tree", resolved_ref])?;

    let mut writes = Vec::new();
    for entry in tree_entries {
        let Some((meta, path)) = entry.split_once('\t') else {
            bail!("unexpected git ls-tree output: {}", entry);
        };
        let parts = meta.split_whitespace().collect::<Vec<_>>();
        if parts.len() != 3 {
            bail!("unexpected git ls-tree metadata: {}", meta);
        }
        let mode = parts[0];
        let object_type = parts[1];
        if object_type != "blob" {
            continue;
        }
        let show_spec = format!("{resolved_ref}:{path}");
        let content = run_git_output(repo, &["show", &show_spec])
            .with_context(|| format!("failed to read blob for {}", path))?;
        writes.push(FileWrite {
            path: path.to_string(),
            content_b64: BASE64.encode(&content),
            executable: mode == "100755",
            mode: git_mode_to_unix_mode(mode),
            checksum_sha256: Some(sha256_hex(&content)),
            preuploaded: false,
            preupload_existed: false,
            preupload_skipped: false,
        });
    }
    Ok(writes)
}

pub fn collect_repo_changes_between_refs(
    repo: &Path,
    base_ref: &str,
    target_ref: &str,
) -> Result<(Vec<FileWrite>, Vec<String>)> {
    let mut changed = BTreeSet::new();
    for line in run_git_lines(
        repo,
        &["diff", "--name-only", "--relative", base_ref, target_ref],
    )? {
        changed.insert(line);
    }

    let mut deleted = BTreeSet::new();
    for line in run_git_lines(
        repo,
        &[
            "diff",
            "--name-only",
            "--diff-filter=D",
            "--relative",
            base_ref,
            target_ref,
        ],
    )? {
        deleted.insert(line);
    }

    let mut writes = Vec::new();
    for relative in changed.difference(&deleted) {
        let show_spec = format!("{target_ref}:{relative}");
        let content = run_git_output(repo, &["show", &show_spec])
            .with_context(|| format!("failed to read blob for {}", relative))?;
        let mode = git_file_mode(repo, target_ref, relative)?;
        writes.push(FileWrite {
            path: relative.clone(),
            content_b64: BASE64.encode(&content),
            executable: mode.as_deref() == Some("100755"),
            mode: mode.as_deref().and_then(git_mode_to_unix_mode),
            checksum_sha256: Some(sha256_hex(&content)),
            preuploaded: false,
            preupload_existed: false,
            preupload_skipped: false,
        });
    }

    Ok((writes, deleted.into_iter().collect()))
}

fn git_file_mode(repo: &Path, resolved_ref: &str, relative: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["ls-tree", resolved_ref, relative])
        .current_dir(repo)
        .output()
        .with_context(|| {
            format!(
                "failed to inspect git mode for {} in {}",
                relative,
                repo.display()
            )
        })?;

    if !output.status.success() {
        bail!(
            "git ls-tree {} {} failed: {}",
            resolved_ref,
            relative,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let stdout = String::from_utf8(output.stdout)?;
    let Some(line) = stdout.lines().find(|line| !line.trim().is_empty()) else {
        return Ok(None);
    };
    let Some((meta, _path)) = line.split_once('\t') else {
        bail!("unexpected git ls-tree output: {}", line);
    };
    let parts = meta.split_whitespace().collect::<Vec<_>>();
    if parts.len() != 3 {
        bail!("unexpected git ls-tree metadata: {}", meta);
    }
    Ok(Some(parts[0].to_string()))
}

fn is_executable(path: &Path) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("failed to stat {}", path.display()))?;
        Ok(metadata.permissions().mode() & 0o111 != 0)
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(false)
    }
}

fn file_mode(path: &Path) -> Result<Option<u32>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("failed to stat {}", path.display()))?;
        Ok(Some(metadata.permissions().mode() & 0o7777))
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(None)
    }
}

fn git_mode_to_unix_mode(mode: &str) -> Option<u32> {
    match mode {
        "100755" => Some(0o755),
        "100644" => Some(0o644),
        _ => None,
    }
}

fn sha256_hex(content: &[u8]) -> String {
    let digest = Sha256::digest(content);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}
