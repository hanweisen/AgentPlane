use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, anyhow};

use crate::server::{self as server_impl, ServerLimits, TlsConfig, TlsMode};

use super::{ServerArgs, TlsModeArg};

pub(super) async fn server(args: ServerArgs) -> Result<ExitCode> {
    let limits = ServerLimits {
        max_processes: args.max_processes,
        max_zombie_processes: args.max_zombie_processes,
        default_process_output_limit_bytes: args.default_process_output_limit_bytes,
        max_process_output_limit_bytes: args.max_process_output_limit_bytes,
        default_process_read_max_bytes: args.default_process_read_max_bytes,
        max_process_read_max_bytes: args.max_process_read_max_bytes,
        max_stdin_write_bytes: args.max_stdin_write_bytes,
        max_process_timeout_seconds: args.max_process_timeout_seconds,
        zombie_ttl_seconds: args.zombie_ttl_seconds,
        default_kill_tree_on_terminate: args.default_kill_tree_on_terminate,
    };
    let tls = match args.tls_mode {
        TlsModeArg::Off => TlsConfig { mode: TlsMode::Off },
        TlsModeArg::SelfSigned => TlsConfig {
            mode: TlsMode::SelfSigned {
                state_dir: args
                    .tls_state_dir
                    .unwrap_or_else(|| PathBuf::from(".agentplane-tls")),
            },
        },
        TlsModeArg::Files => TlsConfig {
            mode: TlsMode::Files {
                cert_path: args
                    .tls_cert
                    .ok_or_else(|| anyhow!("--tls-cert is required when --tls-mode files"))?,
                key_path: args
                    .tls_key
                    .ok_or_else(|| anyhow!("--tls-key is required when --tls-mode files"))?,
            },
        },
    };
    server_impl::serve_with_config_and_accelerators(
        args.listen,
        args.port,
        args.allow_root,
        args.token,
        limits,
        tls,
        args.nvidia_smi_path,
        args.npu_smi_path,
    )
    .await
}
