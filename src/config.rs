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
        help = "Load AP_SERVER, AP_TOKEN, AP_REMOTE_ROOT, AP_HEADER[_N], and retry settings from a KEY=VALUE file."
    )]
    pub(crate) profile: Option<PathBuf>,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct ClientAuthArgs {
    #[arg(long)]
    server: Option<String>,
    #[arg(long)]
    token: Option<String>,
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
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ClientProfile {
    pub(crate) server: Option<String>,
    token: Option<String>,
    remote_root: Option<PathBuf>,
    pub(crate) headers: Vec<String>,
    pub(crate) connect_retries: Option<usize>,
    pub(crate) connect_retry_delay_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedClientAuth {
    pub(crate) server: String,
    pub(crate) token: String,
    pub(crate) request_timeout_seconds: u64,
    pub(crate) connect_retries: usize,
    pub(crate) connect_retry_delay_ms: u64,
    pub(crate) tls_ca_cert: Option<PathBuf>,
    pub(crate) tls_insecure_skip_verify: bool,
    pub(crate) header: Vec<String>,
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
        Ok(ResolvedClientAuth {
            server,
            token,
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
