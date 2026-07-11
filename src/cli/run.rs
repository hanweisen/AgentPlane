//! `run-show` / `run-manifest`: cross-profile aggregation by `run_id`.
//!
//! A run is a client-side grouping label (see `.claude/design/07-run-id-aggregation-design.md`).
//! These subcommands query one or more profiles' `process-list` (filtered
//! server-side by `run_id`), join each process with its `save_output_path`, and
//! present a per-node view. The manifest is a local cache; the servers remain
//! the source of truth, and `--rebuild` reconstructs the cache from server
//! state alone.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::cli_client::{build_http_client, post_json_with_client};
use crate::config::{ResolvedClientAuth, load_client_profile, resolve_profile_auth};
use crate::protocol::{ProcessInfo, ProcessListRequest, ProcessListResponse};

use super::{RunManifestArgs, RunShowArgs};

/// Directory used to cache per-run manifests. Overridable via `AP_RUN_DIR`.
const DEFAULT_RUN_DIR: &str = ".agentplane/runs";

pub(super) async fn run_show(args: RunShowArgs) -> Result<ExitCode> {
    let manifest = build_manifest(&args.run_id, &args.profile, args.rebuild).await?;
    write_manifest_cache(&manifest).await?;
    if args.text {
        print_manifest_text(&manifest);
    } else {
        println!("{}", serde_json::to_string_pretty(&manifest)?);
    }
    Ok(ExitCode::SUCCESS)
}

pub(super) async fn run_manifest(args: RunManifestArgs) -> Result<ExitCode> {
    let manifest = build_manifest(&args.run_id, &args.profile, false).await?;
    let json = serde_json::to_string_pretty(&manifest)?;
    match args.out.as_ref() {
        Some(path) => {
            write_manifest_to(path, &json)?;
            println!(
                "wrote manifest for {} to {}",
                manifest.run_id,
                path.display()
            );
        }
        None => println!("{json}"),
    }
    Ok(ExitCode::SUCCESS)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RunManifest {
    pub run_id: String,
    pub created_at_unix_ms: u128,
    pub nodes: Vec<RunNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RunNode {
    pub label: Option<String>,
    pub server: String,
    pub profile: Option<PathBuf>,
    pub processes: Vec<ProcessInfo>,
}

/// Build the manifest for a run by querying each profile. With `rebuild`,
/// only server state is used; otherwise the local cache is read first and
/// refreshed from server state.
async fn build_manifest(run_id: &str, profiles: &[PathBuf], rebuild: bool) -> Result<RunManifest> {
    let profile_paths = if profiles.is_empty() {
        if rebuild {
            bail!(
                "run-show --rebuild requires at least one --profile (no cached manifest to read)"
            );
        }
        read_cached_profiles(run_id)?
    } else {
        profiles.to_vec()
    };

    let mut nodes = Vec::with_capacity(profile_paths.len());
    for path in &profile_paths {
        let profile = load_client_profile(Some(path))?;
        let auth = resolve_profile_auth(&profile)?;
        let processes = list_run_processes(&auth, run_id).await?;
        nodes.push(RunNode {
            label: profile.label.clone(),
            server: auth.server.clone(),
            profile: Some(path.clone()),
            processes,
        });
    }
    Ok(RunManifest {
        run_id: run_id.to_string(),
        created_at_unix_ms: unix_now_ms(),
        nodes,
    })
}

/// Query one profile's `/v1/process/list` filtered by `run_id`. Returns an
/// empty vector (not an error) if the profile has no processes in this run.
async fn list_run_processes(auth: &ResolvedClientAuth, run_id: &str) -> Result<Vec<ProcessInfo>> {
    let client = build_http_client(auth)?;
    let payload = ProcessListRequest {
        run_id: Some(run_id.to_string()),
    };
    let response = post_json_with_client(&client, auth, "/v1/process/list", &payload, true).await?;
    if response.status() != StatusCode::OK {
        return Err(anyhow!(
            "process-list failed for {} (status {}): {}",
            auth.server,
            response.status(),
            response.text().await.unwrap_or_default()
        ));
    }
    let body: ProcessListResponse = response.json().await?;
    Ok(body.processes)
}

fn print_manifest_text(manifest: &RunManifest) {
    let total: usize = manifest.nodes.iter().map(|node| node.processes.len()).sum();
    println!(
        "run {} ({} process(es), {} node(s))",
        manifest.run_id,
        total,
        manifest.nodes.len()
    );
    for node in &manifest.nodes {
        let label = node.label.as_deref().unwrap_or("?");
        println!(
            "  [{}] server={} processes={}",
            label,
            node.server,
            node.processes.len()
        );
        for process in &node.processes {
            let exit = process
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "-".to_string());
            let save = process.save_output_path.as_deref().unwrap_or("-");
            println!(
                "    {:<24} status={:<8} exit={} save={}",
                process.process_id, process.status, exit, save
            );
        }
    }
}

// ---- manifest cache ----

fn run_dir() -> PathBuf {
    std::env::var("AP_RUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(DEFAULT_RUN_DIR)
        })
}

fn manifest_cache_path(run_id: &str) -> Result<PathBuf> {
    let mut path = run_dir();
    path.push(sanitize_run_id(run_id)?);
    path.set_extension("json");
    Ok(path)
}

/// Only allow run_ids that are safe filename fragments. Rejects path
/// separators and traversal so a malicious `run_id` cannot escape the cache dir.
fn sanitize_run_id(run_id: &str) -> Result<String> {
    if run_id.is_empty() {
        bail!("run_id must not be empty");
    }
    if run_id.contains('/') || run_id.contains('\\') || run_id.contains("..") || run_id == "." {
        bail!("run_id must not contain path separators or traversal: {run_id}");
    }
    Ok(run_id.to_string())
}

async fn write_manifest_cache(manifest: &RunManifest) -> Result<()> {
    let path = manifest_cache_path(&manifest.run_id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create manifest dir {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(manifest)?;
    std::fs::write(&path, json)
        .with_context(|| format!("failed to write manifest {}", path.display()))?;
    Ok(())
}

fn write_manifest_to(path: &Path, json: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create manifest dir {}", parent.display()))?;
    }
    std::fs::write(path, json)
        .with_context(|| format!("failed to write manifest {}", path.display()))?;
    Ok(())
}

/// Read the profiles recorded in a cached manifest (used when no `--profile`
/// is passed). Returns an error if the cache is missing.
fn read_cached_profiles(run_id: &str) -> Result<Vec<PathBuf>> {
    let path = manifest_cache_path(run_id)?;
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "no --profile given and no cached manifest at {}",
            path.display()
        )
    })?;
    let manifest: RunManifest = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse cached manifest {}", path.display()))?;
    let profiles = manifest
        .nodes
        .into_iter()
        .filter_map(|node| node.profile)
        .collect::<Vec<_>>();
    if profiles.is_empty() {
        bail!("cached manifest for {} recorded no profiles", run_id);
    }
    Ok(profiles)
}

fn unix_now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
