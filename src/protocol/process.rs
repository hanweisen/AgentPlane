use serde::{Deserialize, Serialize};

use super::{AcceleratorKind, ResourceClaim};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessStartRequest {
    pub remote_root: String,
    pub process_id: String,
    pub command: Vec<String>,
    pub cwd: Option<String>,
    pub env: Option<std::collections::BTreeMap<String, Option<String>>>,
    #[serde(default)]
    pub claims: Vec<ResourceClaim>,
    pub timeout_seconds: Option<u64>,
    pub output_bytes_limit: Option<usize>,
    pub pipe_stdin: bool,
    #[serde(default)]
    pub kill_tree_on_terminate: bool,
    #[serde(default)]
    pub save_output_path: Option<String>,
    /// Optional grouping label tying related processes together across one or
    /// more nodes (feedback §7). Free-form client-chosen string; the server
    /// stores and echoes it without validating its format. A retry with the
    /// same `process_id` but a different `run_id` is rejected (reconnect-safe).
    #[serde(default)]
    pub run_id: Option<String>,
}

/// Filtering options for `process-list`. All fields optional and
/// `#[serde(default)]` so an older client sending `{}` is unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ProcessListRequest {
    /// When set, only return processes whose `run_id` equals this value.
    #[serde(default)]
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProcessStartConfig<'a> {
    remote_root: &'a str,
    cwd: &'a str,
    command: &'a [String],
    claims: &'a [ResourceClaim],
    pipe_stdin: bool,
    kill_tree_on_terminate: bool,
    save_output_path: Option<&'a str>,
    run_id: Option<&'a str>,
    timeout_seconds: Option<u64>,
    output_bytes_limit: usize,
}

impl<'a> ProcessStartConfig<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        remote_root: &'a str,
        cwd: &'a str,
        command: &'a [String],
        claims: &'a [ResourceClaim],
        pipe_stdin: bool,
        kill_tree_on_terminate: bool,
        save_output_path: Option<&'a str>,
        run_id: Option<&'a str>,
        timeout_seconds: Option<u64>,
        output_bytes_limit: usize,
    ) -> Self {
        Self {
            remote_root,
            cwd,
            command,
            claims,
            pipe_stdin,
            kill_tree_on_terminate,
            save_output_path,
            run_id,
            timeout_seconds,
            output_bytes_limit,
        }
    }
}

impl ProcessStartRequest {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn matches_existing_normalized_config(
        &self,
        existing: &ProcessStartConfig<'_>,
        remote_root: &str,
        cwd: &str,
        claims: &[ResourceClaim],
        output_bytes_limit: usize,
        kill_tree_on_terminate: bool,
        save_output_path: Option<&str>,
        run_id: Option<&str>,
    ) -> bool {
        existing.remote_root == remote_root
            && existing.cwd == cwd
            && existing.command == self.command.as_slice()
            && existing.claims == claims
            && existing.pipe_stdin == self.pipe_stdin
            && existing.kill_tree_on_terminate == kill_tree_on_terminate
            && existing.save_output_path == save_output_path
            && existing.run_id == run_id
            && existing.timeout_seconds == self.timeout_seconds
            && existing.output_bytes_limit == output_bytes_limit
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessStartResponse {
    pub ok: bool,
    pub process_id: String,
    pub created: bool,
    pub already_exists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessGetRequest {
    pub process_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessGetResponse {
    pub ok: bool,
    pub process: ProcessInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessListResponse {
    pub ok: bool,
    pub processes: Vec<ProcessInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessInfo {
    pub process_id: String,
    pub remote_root: String,
    pub cwd: String,
    pub command: Vec<String>,
    pub pipe_stdin: bool,
    pub kill_tree_on_terminate: bool,
    pub process_group_id: Option<i32>,
    #[serde(default)]
    pub children_running: bool,
    pub timeout_seconds: Option<u64>,
    pub output_bytes_limit: usize,
    pub started_at_unix_ms: u128,
    pub finished_at_unix_ms: Option<u128>,
    pub exited: bool,
    pub exit_code: Option<i32>,
    pub failure: Option<String>,
    pub next_seq: u64,
    pub available_from_seq: u64,
    pub truncated: bool,
    pub output_retained: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub pid: Option<i32>,
    #[serde(default)]
    pub elapsed_ms: u128,
    #[serde(default)]
    pub last_output_at_unix_ms: Option<u128>,
    #[serde(default)]
    pub save_output_path: Option<String>,
    /// Echoed run grouping label (feedback §7). Absent when no `--run-id` was
    /// set, so old responses deserialize unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessReadRequest {
    pub process_id: String,
    pub after_seq: Option<u64>,
    pub max_bytes: Option<usize>,
    pub wait_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessReadResponse {
    pub ok: bool,
    pub process_id: String,
    pub chunks: Vec<ProcessOutputChunk>,
    pub next_seq: u64,
    pub available_from_seq: u64,
    pub cursor_expired: bool,
    pub exited: bool,
    pub exit_code: Option<i32>,
    pub truncated: bool,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessEventSubscribeRequest {
    pub process_id: String,
    pub after_seq: Option<u64>,
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProcessEventMessage {
    Read { response: ProcessReadResponse },
    Error { error: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessOutputChunk {
    pub seq: u64,
    pub stream: ProcessOutputStream,
    pub data_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessWriteRequest {
    pub process_id: String,
    pub data_b64: String,
    pub close_stdin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessTerminateRequest {
    pub process_id: String,
    #[serde(default)]
    pub tree: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessCleanupRequest {
    pub process_match: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub kill: bool,
    #[serde(default)]
    pub signal: Option<String>,
    /// When true on a kill request, the server polls the matcher after
    /// signaling and sets `verified` according to whether signaled PIDs exited.
    #[serde(default)]
    pub reconfirm: bool,
    #[serde(default)]
    pub reconfirm_wait_ms: Option<u64>,
    /// When set on a dry-run, the server attaches an accelerator occupancy
    /// summary (per-PID device + memory) of the given kind. Ignored for --kill.
    #[serde(default)]
    pub accelerator_summary: Option<AcceleratorKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessCleanupResponse {
    pub ok: bool,
    pub dry_run: bool,
    pub signal: Option<String>,
    pub matched: Vec<CleanupProcess>,
    pub signaled: Vec<CleanupProcess>,
    pub skipped: Vec<CleanupProcess>,
    /// True only when a kill request requested reconfirmation and all signaled
    /// PIDs were absent from the bounded follow-up scan.
    #[serde(default)]
    pub verified: bool,
    pub agent_hint: String,
    /// Present only when a dry-run requested an accelerator occupancy summary.
    /// `available: false` means the provider could not be queried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accelerator_summary: Option<ProcessCleanupAcceleratorSummary>,
}

/// Per-PID accelerator occupancy attached to a dry-run cleanup report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessCleanupAcceleratorSummary {
    pub kind: AcceleratorKind,
    pub available: bool,
    pub reason: Option<String>,
    /// Occupancy for matched PIDs that the accelerator provider reports as
    /// holding device memory.
    pub processes: Vec<ProcessCleanupAcceleratorProcess>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessCleanupAcceleratorProcess {
    pub pid: i32,
    pub device_index: Option<u32>,
    #[serde(default)]
    pub device_name: Option<String>,
    pub used_memory_mib: Option<u64>,
    #[serde(default)]
    pub memory_total_mib: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CleanupProcess {
    pub pid: i32,
    pub ppid: Option<i32>,
    pub process_group_id: Option<i32>,
    pub session_id: Option<i32>,
    pub elapsed: Option<String>,
    #[serde(default)]
    pub elapsed_seconds: Option<u64>,
    pub stat: Option<String>,
    pub user: Option<String>,
    pub command: Option<String>,
    pub skip_reason: Option<String>,
}
