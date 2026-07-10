use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use clap::Args;

pub(crate) const DEFAULT_HTTP_TIMEOUT_SECONDS: u64 = 60;
pub(crate) const DEFAULT_CONNECT_RETRIES: usize = 2;
pub(crate) const RETRY_BASE_DELAY_MS: u64 = 250;

#[derive(Debug, Args, Clone, Default)]
pub(crate) struct ClientProfileArgs {
    #[arg(
        long,
        global = true,
        visible_alias = "env-file",
        value_name = "PATH",
        help = "Load AP_SERVER, AP_TOKEN, AP_REMOTE_ROOT, AP_LABEL, AP_HEADER[_N], and retry settings from a KEY=VALUE file."
    )]
    pub(crate) profile: Option<PathBuf>,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct ClientAuthArgs {
    #[arg(long)]
    server: Option<String>,
    #[arg(long)]
    token: Option<String>,
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
    #[arg(long = "tls-ca-cert")]
    tls_ca_cert: Option<PathBuf>,
    #[arg(long = "tls-insecure-skip-verify", default_value_t = false)]
    tls_insecure_skip_verify: bool,
    #[arg(
        long = "header",
        help = "Repeatable raw HTTP header like 'Name: value' added to every request."
    )]
    header: Vec<String>,
    #[arg(long = "agent-id")]
    agent_id: Option<String>,
    #[arg(long = "agent-id-file", value_name = "PATH")]
    agent_id_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ClientProfile {
    pub(crate) server: Option<String>,
    token: Option<String>,
    remote_root: Option<PathBuf>,
    pub(crate) socks5_hostname: Option<String>,
    pub(crate) headers: Vec<String>,
    pub(crate) connect_retries: Option<usize>,
    pub(crate) connect_retry_delay_ms: Option<u64>,
    pub(crate) agent_id: Option<String>,
    pub(crate) agent_id_file: Option<PathBuf>,
    pub(crate) label: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedClientAuth {
    pub(crate) server: String,
    pub(crate) token: String,
    pub(crate) socks5_hostname: Option<String>,
    pub(crate) request_timeout_seconds: u64,
    pub(crate) connect_retries: usize,
    pub(crate) connect_retry_delay_ms: u64,
    pub(crate) tls_ca_cert: Option<PathBuf>,
    pub(crate) tls_insecure_skip_verify: bool,
    pub(crate) header: Vec<String>,
    pub(crate) agent_id: String,
}

impl ClientAuthArgs {
    pub(crate) fn resolve(&self, profile: &ClientProfile) -> Result<ResolvedClientAuth> {
        let server = self
            .server
            .clone()
            .or_else(|| profile.server.clone())
            .ok_or_else(|| anyhow!("--server is required unless AP_SERVER is set in --profile"))?;
        let token = self
            .token
            .clone()
            .or_else(|| profile.token.clone())
            .ok_or_else(|| anyhow!("--token is required unless AP_TOKEN is set in --profile"))?;
        let mut header = profile.headers.clone();
        header.extend(self.header.clone());
        let agent_id = resolve_agent_id(
            self.agent_id.as_deref(),
            self.agent_id_file.as_ref(),
            profile.agent_id.as_deref(),
            profile.agent_id_file.as_ref(),
        )?;
        Ok(ResolvedClientAuth {
            server,
            token,
            socks5_hostname: self
                .socks5_hostname
                .clone()
                .or_else(|| profile.socks5_hostname.clone()),
            request_timeout_seconds: self
                .request_timeout_seconds
                .unwrap_or(DEFAULT_HTTP_TIMEOUT_SECONDS),
            connect_retries: self
                .connect_retries
                .or(profile.connect_retries)
                .unwrap_or(DEFAULT_CONNECT_RETRIES),
            connect_retry_delay_ms: self
                .connect_retry_delay_ms
                .or(profile.connect_retry_delay_ms)
                .unwrap_or(RETRY_BASE_DELAY_MS),
            tls_ca_cert: self.tls_ca_cert.clone(),
            tls_insecure_skip_verify: self.tls_insecure_skip_verify,
            header,
            agent_id,
        })
    }
}

pub(crate) fn load_client_profile(path: Option<&PathBuf>) -> Result<ClientProfile> {
    let Some(path) = path else {
        return Ok(ClientProfile::default());
    };
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read profile {}", path.display()))?;
    let values = parse_profile_values(&text)
        .with_context(|| format!("failed to parse profile {}", path.display()))?;
    let mut profile = ClientProfile {
        server: values.get("AP_SERVER").cloned(),
        token: values.get("AP_TOKEN").cloned(),
        remote_root: values.get("AP_REMOTE_ROOT").map(PathBuf::from),
        socks5_hostname: values.get("AP_SOCKS5_HOSTNAME").cloned(),
        agent_id: values.get("AP_AGENT_ID").cloned(),
        agent_id_file: values.get("AP_AGENT_ID_FILE").map(PathBuf::from),
        label: values.get("AP_LABEL").cloned(),
        ..ClientProfile::default()
    };
    profile.headers.extend(profile_headers(&values));
    if let Some(value) = values.get("AP_CONNECT_RETRIES") {
        profile.connect_retries = Some(
            value
                .parse()
                .with_context(|| format!("invalid AP_CONNECT_RETRIES value: {value}"))?,
        );
    }
    if let Some(value) = values.get("AP_CONNECT_RETRY_DELAY_MS") {
        profile.connect_retry_delay_ms = Some(
            value
                .parse()
                .with_context(|| format!("invalid AP_CONNECT_RETRY_DELAY_MS value: {value}"))?,
        );
    }
    Ok(profile)
}

fn resolve_agent_id(
    cli_agent_id: Option<&str>,
    cli_agent_id_file: Option<&PathBuf>,
    profile_agent_id: Option<&str>,
    profile_agent_id_file: Option<&PathBuf>,
) -> Result<String> {
    if let Some(agent_id) = cli_agent_id {
        return normalize_agent_id(agent_id);
    }
    if let Some(path) = cli_agent_id_file {
        return read_agent_id_file(path);
    }
    if let Some(agent_id) = profile_agent_id {
        return normalize_agent_id(agent_id);
    }
    if let Some(path) = profile_agent_id_file {
        return read_agent_id_file(path);
    }
    Ok(format!("agentplane-{}", std::process::id()))
}

fn read_agent_id_file(path: &PathBuf) -> Result<String> {
    let value = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read agent id file {}", path.display()))?;
    normalize_agent_id(value.trim())
}

fn normalize_agent_id(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("agent id must not be empty"));
    }
    Ok(trimmed.to_string())
}

fn profile_headers(values: &BTreeMap<String, String>) -> Vec<String> {
    values
        .iter()
        .filter(|(key, _)| key == &"AP_HEADER" || key.starts_with("AP_HEADER_"))
        .map(|(_, value)| value.to_string())
        .collect()
}

fn parse_profile_values(text: &str) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for (index, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some((key, value)) = line.split_once('=') else {
            return Err(anyhow!("line {} is not KEY=VALUE: {}", index + 1, raw_line));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(anyhow!("line {} has an empty key", index + 1));
        }
        let value = unquote_profile_value(value.trim());
        values.insert(key.to_string(), value.to_string());
    }
    Ok(values)
}

fn unquote_profile_value(value: &str) -> &str {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
            || (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
        {
            return &value[1..value.len() - 1];
        }
    }
    value
}

pub(crate) fn resolve_remote_root(
    cli_value: Option<&PathBuf>,
    profile: &ClientProfile,
) -> Result<PathBuf> {
    cli_value
        .cloned()
        .or_else(|| profile.remote_root.clone())
        .ok_or_else(|| {
            anyhow!("--remote-root is required unless AP_REMOTE_ROOT is set in --profile")
        })
}

/// Resolve client auth using only values from a profile file, with no CLI
/// overrides. Used by commands such as `file-copy` that drive two independent
/// profiles and therefore cannot take a single set of `--server`/`--token` args.
pub(crate) fn resolve_profile_auth(profile: &ClientProfile) -> Result<ResolvedClientAuth> {
    ClientAuthArgs {
        server: None,
        token: None,
        socks5_hostname: None,
        request_timeout_seconds: None,
        connect_retries: None,
        connect_retry_delay_ms: None,
        tls_ca_cert: None,
        tls_insecure_skip_verify: false,
        header: Vec::new(),
        agent_id: None,
        agent_id_file: None,
    }
    .resolve(profile)
}

pub(crate) fn parse_octal_mode(value: &str) -> std::result::Result<u32, String> {
    let trimmed = value.trim_start_matches("0o");
    if trimmed.is_empty() || trimmed.len() > 4 {
        return Err("mode must be an octal value like 644 or 0755".to_string());
    }
    if !trimmed.bytes().all(|byte| (b'0'..=b'7').contains(&byte)) {
        return Err("mode must contain only octal digits".to_string());
    }
    u32::from_str_radix(trimmed, 8)
        .map_err(|error| error.to_string())
        .and_then(|mode| {
            if mode <= 0o7777 {
                Ok(mode)
            } else {
                Err("mode must be no greater than 7777".to_string())
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_client_profile_reads_socks5_hostname() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("agentplane.env");
        std::fs::write(
            &path,
            "AP_SERVER=http://example.test\nAP_TOKEN=test-token\nAP_SOCKS5_HOSTNAME=127.0.0.1:1086\n",
        )?;

        let profile = load_client_profile(Some(&path))?;
        assert_eq!(profile.server.as_deref(), Some("http://example.test"));
        assert_eq!(profile.socks5_hostname.as_deref(), Some("127.0.0.1:1086"));
        Ok(())
    }

    #[test]
    fn load_client_profile_reads_agent_id_file() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let agent_id_path = dir.path().join("agent.id");
        std::fs::write(&agent_id_path, "minimax-a\n")?;
        let profile_path = dir.path().join("agentplane.env");
        std::fs::write(
            &profile_path,
            format!(
                "AP_SERVER=http://example.test\nAP_TOKEN=test-token\nAP_AGENT_ID_FILE={}\n",
                agent_id_path.display()
            ),
        )?;

        let profile = load_client_profile(Some(&profile_path))?;
        let auth = ClientAuthArgs {
            server: None,
            token: None,
            socks5_hostname: None,
            request_timeout_seconds: None,
            connect_retries: None,
            connect_retry_delay_ms: None,
            tls_ca_cert: None,
            tls_insecure_skip_verify: false,
            header: Vec::new(),
            agent_id: None,
            agent_id_file: None,
        }
        .resolve(&profile)?;
        assert_eq!(auth.agent_id, "minimax-a");
        Ok(())
    }
}
