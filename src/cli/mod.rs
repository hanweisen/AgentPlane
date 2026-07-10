mod accelerator;
mod file;
mod health;
mod mode;
mod process;
mod server;
mod sync;
mod sync_session;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::config::{ClientAuthArgs, ClientProfileArgs, load_client_profile, parse_octal_mode};

#[derive(Debug, Parser)]
#[command(name = "agentplane")]
#[command(
    about = "Operate remote development containers, process sessions, file sync, and accelerators."
)]
pub struct App {
    #[command(flatten)]
    profile: ClientProfileArgs,
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    SyncRun(SyncRunArgs),
    SyncInit(SyncInitArgs),
    Health(HealthArgs),
    Server(ServerArgs),
    AcceleratorStatus(AcceleratorStatusArgs),
    GpuStatus(GpuStatusArgs),
    NpuStatus(NpuStatusArgs),
    AcceleratorPreflight(AcceleratorPreflightArgs),
    GpuPreflight(GpuPreflightArgs),
    AcceleratorWaitIdle(AcceleratorWaitIdleArgs),
    GpuWaitIdle(GpuWaitIdleArgs),
    ProcessStart(ProcessStartArgs),
    ProcessRun(ProcessRunArgs),
    ProcessGet(ProcessGetArgs),
    ProcessList(ProcessListArgs),
    ProcessRead(ProcessReadArgs),
    ProcessWrite(ProcessWriteArgs),
    ProcessTerminate(ProcessTerminateArgs),
    ProcessCleanup(ProcessCleanupArgs),
    ProcessStatus(ProcessStatusArgs),
    ModeGet(ModeGetArgs),
    ModeSwitch(ModeSwitchArgs),
    LeaseRenew(LeaseRenewArgs),
    LeaseRelease(LeaseReleaseArgs),
    FileRead(FileReadArgs),
    FileStat(FileStatArgs),
    FileWait(FileWaitArgs),
    FileWrite(FileWriteArgs),
    FileUpload(FileUploadArgs),
    FileCopy(FileCopyArgs),
    FileDelete(FileDeleteArgs),
    FileFind(FileFindArgs),
    FileList(FileListArgs),
}

#[derive(Debug, Args)]
struct SyncRunArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long)]
    repo: PathBuf,
    #[arg(long = "remote-root")]
    remote_root: Option<PathBuf>,
    #[arg(long = "ref")]
    git_ref: Option<String>,
    #[arg(long = "base-ref")]
    base_ref: Option<String>,
    #[arg(long = "preserve-path")]
    preserve_path: Vec<String>,
    #[arg(long = "exact-sync", default_value_t = false)]
    exact_sync: bool,
    #[arg(long)]
    command: Option<String>,
    #[arg(
        long = "timeout-seconds",
        default_value_t = 600,
        help = "Remote command timeout in seconds."
    )]
    timeout_seconds: u64,
    #[arg(
        long = "env",
        help = "Repeatable KEY=VALUE or KEY= pairs for the remote environment."
    )]
    env: Vec<String>,
    #[arg(
        long = "claim",
        help = "Repeatable resource claim KIND:UNIT[,UNIT...]. Enforced only for command execution in shared mode."
    )]
    claims: Vec<String>,
    #[arg(long = "dry-run", default_value_t = false)]
    dry_run: bool,
    #[arg(long = "checksum", default_value_t = false)]
    checksum: bool,
    #[arg(long = "preserve-mode", default_value_t = false)]
    preserve_mode: bool,
    #[arg(long = "atomic-files", default_value_t = false)]
    atomic_files: bool,
    #[arg(
        long = "upload-chunk-size",
        value_name = "BYTES",
        default_value_t = 256 * 1024,
        help = "Chunk size used by sync-run's file-upload transport."
    )]
    upload_chunk_size: usize,
    #[arg(long = "include")]
    include: Vec<String>,
    #[arg(long = "exclude-from")]
    exclude_from: Vec<PathBuf>,
}

#[derive(Debug, Args)]
struct SyncInitArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long = "remote-root")]
    remote_root: Option<PathBuf>,
    #[arg(long = "preserve-path")]
    preserve_path: Vec<String>,
    #[arg(long = "dry-run", default_value_t = false)]
    dry_run: bool,
    #[arg(long = "checksum", default_value_t = false)]
    checksum: bool,
    #[arg(long = "preserve-mode", default_value_t = false)]
    preserve_mode: bool,
    #[arg(long = "atomic-files", default_value_t = false)]
    atomic_files: bool,
    #[arg(
        long = "upload-chunk-size",
        value_name = "BYTES",
        default_value_t = 256 * 1024,
        help = "Chunk size used by sync-init's file-upload transport."
    )]
    upload_chunk_size: usize,
}

#[derive(Debug, Args)]
struct HealthArgs {
    #[arg(long)]
    server: Option<String>,
    #[arg(
        long = "socks5-hostname",
        value_name = "HOST:PORT|URL",
        help = "Route requests through a SOCKS5 proxy with remote DNS, for example 127.0.0.1:1086 or socks5h://127.0.0.1:1086."
    )]
    socks5_hostname: Option<String>,
    #[arg(long = "request-timeout-seconds")]
    request_timeout_seconds: Option<u64>,
    #[arg(long = "connect-retries")]
    connect_retries: Option<usize>,
    #[arg(
        long = "connect-retry-delay-ms",
        help = "Delay between safe retries after a timeout, connect failure, or retryable gateway response."
    )]
    connect_retry_delay_ms: Option<u64>,
    #[arg(long)]
    token: Option<String>,
    #[arg(long = "tls-ca-cert")]
    tls_ca_cert: Option<PathBuf>,
    #[arg(long = "tls-insecure-skip-verify", default_value_t = false)]
    tls_insecure_skip_verify: bool,
    #[arg(
        long = "header",
        help = "Repeatable raw HTTP header like 'Name: value' added to every request."
    )]
    header: Vec<String>,
}

#[derive(Debug, Args)]
struct ServerArgs {
    #[arg(long, default_value = "127.0.0.1")]
    listen: String,
    #[arg(long, default_value_t = 8765)]
    port: u16,
    #[arg(long = "allow-root", required = true)]
    allow_root: Vec<PathBuf>,
    #[arg(long)]
    token: String,
    #[arg(long = "tls-mode", value_enum, default_value_t = TlsModeArg::Off)]
    tls_mode: TlsModeArg,
    #[arg(long = "tls-state-dir")]
    tls_state_dir: Option<PathBuf>,
    #[arg(long = "tls-cert")]
    tls_cert: Option<PathBuf>,
    #[arg(long = "tls-key")]
    tls_key: Option<PathBuf>,
    #[arg(long = "max-processes", default_value_t = 8)]
    max_processes: usize,
    #[arg(long = "max-zombie-processes", default_value_t = 32)]
    max_zombie_processes: usize,
    #[arg(long = "default-process-output-limit-bytes", default_value_t = 1024 * 1024)]
    default_process_output_limit_bytes: usize,
    #[arg(long = "max-process-output-limit-bytes", default_value_t = 8 * 1024 * 1024)]
    max_process_output_limit_bytes: usize,
    #[arg(long = "default-process-read-max-bytes", default_value_t = 64 * 1024)]
    default_process_read_max_bytes: usize,
    #[arg(long = "max-process-read-max-bytes", default_value_t = 1024 * 1024)]
    max_process_read_max_bytes: usize,
    #[arg(long = "max-stdin-write-bytes", default_value_t = 64 * 1024)]
    max_stdin_write_bytes: usize,
    #[arg(long = "max-process-timeout-seconds", default_value_t = 24 * 60 * 60)]
    max_process_timeout_seconds: u64,
    #[arg(long = "zombie-ttl-seconds", default_value_t = 600)]
    zombie_ttl_seconds: u64,
    #[arg(long = "default-kill-tree-on-terminate", default_value_t = false)]
    default_kill_tree_on_terminate: bool,
    #[arg(long = "nvidia-smi-path")]
    nvidia_smi_path: Option<PathBuf>,
    #[arg(long = "npu-smi-path")]
    npu_smi_path: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct AcceleratorStatusArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "kind", value_enum, default_value_t = AcceleratorKindArg::Gpu)]
    kind: AcceleratorKindArg,
    #[arg(long = "gpus")]
    gpus: Option<String>,
    #[arg(long = "match")]
    process_match: Option<String>,
    #[arg(long = "json", default_value_t = false)]
    json: bool,
    #[arg(long = "text", default_value_t = false)]
    text: bool,
}

#[derive(Debug, Args)]
struct GpuStatusArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "gpus")]
    gpus: Option<String>,
    #[arg(long = "match")]
    process_match: Option<String>,
    #[arg(long = "json", default_value_t = false)]
    json: bool,
    #[arg(long = "text", default_value_t = false)]
    text: bool,
}

#[derive(Debug, Args)]
struct NpuStatusArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "gpus")]
    gpus: Option<String>,
    #[arg(long = "match")]
    process_match: Option<String>,
    #[arg(long = "json", default_value_t = false)]
    json: bool,
    #[arg(long = "text", default_value_t = false)]
    text: bool,
}

#[derive(Debug, Args)]
struct AcceleratorPreflightArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "kind", value_enum, default_value_t = AcceleratorKindArg::Gpu)]
    kind: AcceleratorKindArg,
    #[arg(long = "gpus")]
    gpus: Option<String>,
    #[arg(long = "max-memory-mib", default_value_t = 256)]
    max_memory_mib: u64,
    #[arg(long = "max-util-percent", default_value_t = 5, value_parser = clap::value_parser!(u32).range(0..=100))]
    max_util_percent: u32,
    #[arg(long = "forbid-match")]
    forbid_match: Option<String>,
    #[arg(long = "json", default_value_t = false)]
    json: bool,
    #[arg(long = "text", default_value_t = false)]
    text: bool,
}

#[derive(Debug, Args)]
struct GpuPreflightArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "gpus")]
    gpus: Option<String>,
    #[arg(long = "max-memory-mib", default_value_t = 256)]
    max_memory_mib: u64,
    #[arg(long = "max-util-percent", default_value_t = 5, value_parser = clap::value_parser!(u32).range(0..=100))]
    max_util_percent: u32,
    #[arg(long = "forbid-match")]
    forbid_match: Option<String>,
    #[arg(long = "json", default_value_t = false)]
    json: bool,
    #[arg(long = "text", default_value_t = false)]
    text: bool,
}

#[derive(Debug, Args)]
struct AcceleratorWaitIdleArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "kind", value_enum, default_value_t = AcceleratorKindArg::Gpu)]
    kind: AcceleratorKindArg,
    #[arg(long = "gpus")]
    gpus: Option<String>,
    #[arg(long = "max-memory-mib", default_value_t = 256)]
    max_memory_mib: u64,
    #[arg(long = "max-util-percent", default_value_t = 5, value_parser = clap::value_parser!(u32).range(0..=100))]
    max_util_percent: u32,
    #[arg(long = "forbid-match")]
    forbid_match: Option<String>,
    #[arg(long = "stable-seconds", default_value_t = 10)]
    stable_seconds: u64,
    #[arg(long = "timeout-seconds", default_value_t = 180)]
    timeout_seconds: u64,
    #[arg(long = "poll-ms", default_value_t = 1000)]
    poll_ms: u64,
    #[arg(long = "json", default_value_t = false)]
    json: bool,
    #[arg(long = "text", default_value_t = false)]
    text: bool,
}

#[derive(Debug, Args)]
struct GpuWaitIdleArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "gpus")]
    gpus: Option<String>,
    #[arg(long = "max-memory-mib", default_value_t = 256)]
    max_memory_mib: u64,
    #[arg(long = "max-util-percent", default_value_t = 5, value_parser = clap::value_parser!(u32).range(0..=100))]
    max_util_percent: u32,
    #[arg(long = "forbid-match")]
    forbid_match: Option<String>,
    #[arg(long = "stable-seconds", default_value_t = 10)]
    stable_seconds: u64,
    #[arg(long = "timeout-seconds", default_value_t = 180)]
    timeout_seconds: u64,
    #[arg(long = "poll-ms", default_value_t = 1000)]
    poll_ms: u64,
    #[arg(long = "json", default_value_t = false)]
    json: bool,
    #[arg(long = "text", default_value_t = false)]
    text: bool,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Agent usage:\n  Use process-start for long-running producers, samplers, servers, and benchmarks that should keep running while you do other work.\n  Use process-run for short build/check commands and consumers/drivers where the local exit code should match the remote command."
)]
struct ProcessStartArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "remote-root")]
    remote_root: Option<PathBuf>,
    #[arg(long = "process-id")]
    process_id: String,
    #[arg(long = "cwd")]
    cwd: Option<String>,
    #[arg(long = "timeout-seconds", help = "Remote command timeout in seconds.")]
    timeout_seconds: Option<u64>,
    #[arg(long = "output-bytes-limit")]
    output_bytes_limit: Option<usize>,
    #[arg(
        long = "save-output-path",
        help = "Remote-root-relative path that receives the full stdout/stderr stream."
    )]
    save_output_path: Option<String>,
    #[arg(
        long = "env",
        help = "Repeatable KEY=VALUE or KEY= pairs for the remote environment."
    )]
    env: Vec<String>,
    #[arg(
        long = "claim",
        help = "Repeatable resource claim KIND:UNIT[,UNIT...]. Enforced only for command execution in shared mode."
    )]
    claims: Vec<String>,
    #[arg(long = "pipe-stdin", default_value_t = false)]
    pipe_stdin: bool,
    #[arg(
        long = "kill-tree-on-terminate",
        default_value_t = false,
        help = "Start the process in its own process group and terminate that group when requested."
    )]
    kill_tree_on_terminate: bool,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
    command: Vec<String>,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Agent usage:\n  Use process-run for short build/check commands and consumers/drivers where the local exit code should match the remote command.\n  Use process-start for long-running producers, samplers, servers, and benchmarks that should keep running while you do other work."
)]
struct ProcessRunArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "remote-root")]
    remote_root: Option<PathBuf>,
    #[arg(long = "process-id")]
    process_id: String,
    #[arg(long = "cwd")]
    cwd: Option<String>,
    #[arg(long = "timeout-seconds", help = "Remote command timeout in seconds.")]
    timeout_seconds: Option<u64>,
    #[arg(long = "output-bytes-limit")]
    output_bytes_limit: Option<usize>,
    #[arg(
        long = "save-output-path",
        help = "Remote-root-relative path that receives the full stdout/stderr stream."
    )]
    save_output_path: Option<String>,
    #[arg(long = "max-bytes")]
    max_bytes: Option<usize>,
    #[arg(long = "wait-ms", default_value_t = 1000)]
    wait_ms: u64,
    #[arg(
        long = "env",
        help = "Repeatable KEY=VALUE or KEY= pairs for the remote environment."
    )]
    env: Vec<String>,
    #[arg(
        long = "claim",
        help = "Repeatable resource claim KIND:UNIT[,UNIT...]. Enforced only for command execution in shared mode."
    )]
    claims: Vec<String>,
    #[arg(
        long = "json",
        default_value_t = false,
        help = "Print the final process-read response as JSON instead of streaming decoded text."
    )]
    json: bool,
    #[arg(
        long = "tail-on-error",
        value_name = "BYTES",
        help = "When the remote command exits non-zero, print the last retained output bytes to stderr."
    )]
    tail_on_error: Option<usize>,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
    command: Vec<String>,
}

#[derive(Debug, Args)]
struct ProcessReadArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "process-id")]
    process_id: String,
    #[arg(long = "after-seq")]
    after_seq: Option<u64>,
    #[arg(long = "max-bytes")]
    max_bytes: Option<usize>,
    #[arg(long = "wait-ms")]
    wait_ms: Option<u64>,
    #[arg(
        long = "text",
        default_value_t = false,
        help = "Print decoded text output instead of JSON."
    )]
    text: bool,
    #[arg(
        long = "follow",
        default_value_t = false,
        help = "Keep polling until the process exits."
    )]
    follow: bool,
    #[arg(
        long = "tail",
        default_value_t = false,
        help = "Start reading from the current end of the log."
    )]
    tail: bool,
}

#[derive(Debug, Args)]
struct ProcessGetArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "process-id")]
    process_id: String,
}

#[derive(Debug, Args)]
struct ProcessListArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
}

#[derive(Debug, Args)]
struct ProcessWriteArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "process-id")]
    process_id: String,
    #[arg(long = "data")]
    data: String,
    #[arg(long = "close-stdin", default_value_t = false)]
    close_stdin: bool,
}

#[derive(Debug, Args)]
struct ProcessTerminateArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "process-id")]
    process_id: String,
    #[arg(
        long = "tree",
        default_value_t = false,
        help = "Terminate the process group created by --kill-tree-on-terminate."
    )]
    tree: bool,
}

#[derive(Debug, Args)]
struct ProcessCleanupArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(
        long = "match",
        help = "Required process command substring. Use 'a|b|c' for multiple terms."
    )]
    process_match: String,
    #[arg(long = "dry-run", default_value_t = false)]
    dry_run: bool,
    #[arg(long = "kill", default_value_t = false)]
    kill: bool,
    #[arg(long = "signal", value_name = "TERM|KILL")]
    signal: Option<String>,
    #[arg(long = "json", default_value_t = false)]
    json: bool,
    #[arg(long = "text", default_value_t = false)]
    text: bool,
}

#[derive(Debug, Args)]
struct ProcessStatusArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "process-id")]
    process_id: Option<String>,
    #[arg(long = "limit", default_value_t = 10)]
    limit: usize,
    #[arg(long = "text", default_value_t = false)]
    text: bool,
}

#[derive(Debug, Args)]
struct ModeGetArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
}

#[derive(Debug, Args)]
struct ModeSwitchArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "mode", value_enum)]
    mode: AgentModeArg,
    #[arg(long = "task-id")]
    task_id: Option<String>,
    #[arg(long = "lease-id")]
    lease_id: Option<String>,
    #[arg(long = "ttl-seconds")]
    ttl_seconds: Option<u64>,
    #[arg(long = "heartbeat-seconds")]
    heartbeat_seconds: Option<u64>,
    #[arg(long = "max-renewals")]
    max_renewals: Option<u32>,
}

#[derive(Debug, Args)]
struct LeaseRenewArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "task-id")]
    task_id: String,
    #[arg(long = "lease-id")]
    lease_id: String,
}

#[derive(Debug, Args)]
struct LeaseReleaseArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "task-id")]
    task_id: String,
    #[arg(long = "lease-id")]
    lease_id: String,
}

#[derive(Debug, Args)]
struct FileReadArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "remote-root")]
    remote_root: Option<PathBuf>,
    #[arg(long = "path")]
    path: String,
    #[arg(
        long = "text",
        default_value_t = false,
        help = "Print decoded text directly instead of JSON."
    )]
    text: bool,
}

#[derive(Debug, Args)]
struct FileStatArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "remote-root")]
    remote_root: Option<PathBuf>,
    #[arg(long = "path")]
    path: String,
}

#[derive(Debug, Args)]
struct FileWaitArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "remote-root")]
    remote_root: Option<PathBuf>,
    #[arg(long = "path")]
    path: String,
    #[arg(long = "min-bytes")]
    min_bytes: Option<u64>,
    #[arg(long = "stable-ms")]
    stable_ms: Option<u64>,
    #[arg(long = "timeout-seconds", default_value_t = 60)]
    timeout_seconds: u64,
}

#[derive(Debug, Args)]
struct FileWriteArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "remote-root")]
    remote_root: Option<PathBuf>,
    #[arg(long = "path")]
    path: String,
    #[arg(long = "content", conflicts_with = "from_local")]
    content: Option<String>,
    #[arg(long = "from-local", value_name = "PATH")]
    from_local: Option<PathBuf>,
    #[arg(long = "executable", default_value_t = false)]
    executable: bool,
    #[arg(long = "create-parents", default_value_t = true)]
    create_parents: bool,
    #[arg(long = "atomic", default_value_t = false)]
    atomic: bool,
    #[arg(long = "mode", value_parser = parse_octal_mode)]
    mode: Option<u32>,
    #[arg(long = "preserve-mode", default_value_t = false)]
    preserve_mode: bool,
    #[arg(long = "checksum", value_name = "SHA256")]
    checksum_sha256: Option<String>,
}

#[derive(Debug, Args)]
struct FileUploadArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "remote-root")]
    remote_root: Option<PathBuf>,
    #[arg(long = "path")]
    path: String,
    #[arg(long = "from-local", value_name = "PATH")]
    from_local: PathBuf,
    #[arg(long = "chunk-size", value_name = "BYTES", default_value_t = 1024 * 1024)]
    chunk_size: usize,
    #[arg(long = "resume", default_value_t = false)]
    resume: bool,
    #[arg(long = "executable", default_value_t = false)]
    executable: bool,
    #[arg(long = "create-parents", default_value_t = true)]
    create_parents: bool,
    #[arg(long = "atomic", default_value_t = false)]
    atomic: bool,
    #[arg(long = "mode", value_parser = parse_octal_mode)]
    mode: Option<u32>,
    #[arg(long = "preserve-mode", default_value_t = false)]
    preserve_mode: bool,
    #[arg(long = "checksum", value_name = "SHA256")]
    checksum_sha256: Option<String>,
    #[arg(long = "lock-key")]
    lock_key: Option<String>,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Agent usage:\n  Use file-copy to move a single file between two profiles (for example node13 <-> node14) in one step, instead of file-read to a local temp file, file-write to the other node, and cleaning up by hand. Each side is addressed by its own --profile file; no tokens are passed on the command line."
)]
struct FileCopyArgs {
    #[arg(
        long = "from-profile",
        value_name = "PATH",
        help = "Profile file (KEY=VALUE) describing the source server."
    )]
    from_profile: PathBuf,
    #[arg(
        long = "to-profile",
        value_name = "PATH",
        help = "Profile file (KEY=VALUE) describing the destination server."
    )]
    to_profile: PathBuf,
    #[arg(
        long = "from-path",
        value_name = "REMOTE_REL_PATH",
        help = "Source file path relative to the source remote root."
    )]
    from_path: String,
    #[arg(
        long = "to-path",
        value_name = "REMOTE_REL_PATH",
        help = "Destination file path relative to the destination remote root."
    )]
    to_path: String,
    #[arg(
        long = "from-remote-root",
        value_name = "PATH",
        help = "Override the source profile's AP_REMOTE_ROOT."
    )]
    from_remote_root: Option<PathBuf>,
    #[arg(
        long = "to-remote-root",
        value_name = "PATH",
        help = "Override the destination profile's AP_REMOTE_ROOT."
    )]
    to_remote_root: Option<PathBuf>,
    #[arg(
        long = "chunk-size",
        value_name = "BYTES",
        default_value_t = 1024 * 1024,
        help = "Chunk size used by the chunked upload to the destination."
    )]
    chunk_size: usize,
    #[arg(
        long = "atomic",
        default_value_t = false,
        help = "Write the destination file atomically."
    )]
    atomic: bool,
    #[arg(
        long = "checksum",
        default_value_t = false,
        help = "After copying, stat the destination and verify its SHA-256 matches the source."
    )]
    checksum: bool,
}

#[derive(Debug, Args)]
struct FileDeleteArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "remote-root")]
    remote_root: Option<PathBuf>,
    #[arg(long = "path")]
    path: String,
}

#[derive(Debug, Args)]
struct FileFindArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "remote-root")]
    remote_root: Option<PathBuf>,
    #[arg(long = "pattern")]
    pattern: String,
}

#[derive(Debug, Args)]
struct FileListArgs {
    #[command(flatten)]
    auth: ClientAuthArgs,
    #[arg(long = "remote-root")]
    remote_root: Option<PathBuf>,
    #[arg(long = "path")]
    path: Option<String>,
}

#[derive(Debug, Clone, clap::ValueEnum)]
enum TlsModeArg {
    Off,
    SelfSigned,
    Files,
}

#[derive(Debug, Clone, clap::ValueEnum)]
enum AgentModeArg {
    Single,
    Shared,
}

#[derive(Debug, Clone, clap::ValueEnum)]
enum AcceleratorKindArg {
    Gpu,
    Npu,
}

pub async fn run() -> Result<ExitCode> {
    let app = App::parse();
    let profile = load_client_profile(app.profile.profile.as_ref())?;
    match app.command {
        CommandKind::SyncRun(args) => sync::sync_run(args, &profile).await,
        CommandKind::SyncInit(args) => sync::sync_init(args, &profile).await,
        CommandKind::Health(args) => health::health(args, &profile).await,
        CommandKind::Server(args) => server::server(args).await,
        CommandKind::AcceleratorStatus(args) => {
            accelerator::accelerator_status(args, &profile).await
        }
        CommandKind::GpuStatus(args) => accelerator::gpu_status(args, &profile).await,
        CommandKind::NpuStatus(args) => accelerator::npu_status(args, &profile).await,
        CommandKind::AcceleratorPreflight(args) => {
            accelerator::accelerator_preflight(args, &profile).await
        }
        CommandKind::GpuPreflight(args) => accelerator::gpu_preflight(args, &profile).await,
        CommandKind::AcceleratorWaitIdle(args) => {
            accelerator::accelerator_wait_idle(args, &profile).await
        }
        CommandKind::GpuWaitIdle(args) => accelerator::gpu_wait_idle(args, &profile).await,
        CommandKind::ProcessStart(args) => process::process_start(args, &profile).await,
        CommandKind::ProcessRun(args) => process::process_run(args, &profile).await,
        CommandKind::ProcessGet(args) => process::process_get(args, &profile).await,
        CommandKind::ProcessList(args) => process::process_list(args, &profile).await,
        CommandKind::ProcessRead(args) => process::process_read(args, &profile).await,
        CommandKind::ProcessWrite(args) => process::process_write(args, &profile).await,
        CommandKind::ProcessTerminate(args) => process::process_terminate(args, &profile).await,
        CommandKind::ProcessCleanup(args) => process::process_cleanup(args, &profile).await,
        CommandKind::ProcessStatus(args) => process::process_status(args, &profile).await,
        CommandKind::ModeGet(args) => mode::mode_get(args, &profile).await,
        CommandKind::ModeSwitch(args) => mode::mode_switch(args, &profile).await,
        CommandKind::LeaseRenew(args) => mode::lease_renew(args, &profile).await,
        CommandKind::LeaseRelease(args) => mode::lease_release(args, &profile).await,
        CommandKind::FileRead(args) => file::file_read(args, &profile).await,
        CommandKind::FileStat(args) => file::file_stat(args, &profile).await,
        CommandKind::FileWait(args) => file::file_wait(args, &profile).await,
        CommandKind::FileWrite(args) => file::file_write(args, &profile).await,
        CommandKind::FileUpload(args) => file::file_upload(args, &profile).await,
        CommandKind::FileCopy(args) => file::file_copy(args, &profile).await,
        CommandKind::FileDelete(args) => file::file_delete(args, &profile).await,
        CommandKind::FileFind(args) => file::file_find(args, &profile).await,
        CommandKind::FileList(args) => file::file_list(args, &profile).await,
    }
}
