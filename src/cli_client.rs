use std::error::Error as StdError;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Certificate, Client, ClientBuilder, Proxy, StatusCode};

use crate::config::ResolvedClientAuth;
use crate::protocol::{
    ProcessGetRequest, ProcessGetResponse, ProcessStartRequest, ProcessStartResponse,
    SimpleResponse,
};

const DEFAULT_CONNECT_TIMEOUT_SECONDS: u64 = 10;

pub(crate) async fn post_json<T: serde::Serialize>(
    auth: &ResolvedClientAuth,
    path: &str,
    payload: &T,
    retry_safe: bool,
) -> Result<reqwest::Response> {
    let client = build_http_client(auth)?;
    post_json_with_client(&client, auth, path, payload, retry_safe).await
}

pub(crate) async fn post_json_with_client<T: serde::Serialize>(
    client: &Client,
    auth: &ResolvedClientAuth,
    path: &str,
    payload: &T,
    retry_safe: bool,
) -> Result<reqwest::Response> {
    let client = client.clone();
    let url = format!("{}{}", normalize_server_url(&auth.server), path);
    let authorization = format!("Bearer {}", auth.token);
    send_with_retry(
        &auth.server,
        auth.socks5_hostname.as_deref(),
        auth.connect_retries,
        auth.connect_retry_delay_ms,
        retry_safe,
        || {
            Ok(client
                .post(&url)
                .header(AUTHORIZATION, authorization.clone())
                .json(payload))
        },
    )
    .await
}

pub(crate) async fn post_process_start_with_recovery(
    auth: &ResolvedClientAuth,
    payload: &ProcessStartRequest,
) -> Result<ProcessStartResponse> {
    let max_attempts = auth.connect_retries.saturating_add(1);
    for attempt in 0..max_attempts {
        let response = post_json(auth, "/v1/process/start", payload, false).await;
        match response {
            Ok(response) => {
                let status = response.status();
                if status == StatusCode::OK {
                    let body: ProcessStartResponse = response.json().await?;
                    return Ok(body);
                }
                if should_retry_response_status(status) {
                    if let Some(recovered) =
                        try_recover_existing_process_start(auth, payload).await?
                    {
                        return Ok(recovered);
                    }
                    let has_more_attempts = attempt + 1 < max_attempts;
                    if has_more_attempts {
                        tokio::time::sleep(Duration::from_millis(auth.connect_retry_delay_ms))
                            .await;
                        continue;
                    }
                }
                return Err(process_error_response(response).await);
            }
            Err(error) => {
                if !is_retryable_process_start_transport_error(&error) {
                    return Err(error);
                }
                if let Some(recovered) = try_recover_existing_process_start(auth, payload).await? {
                    return Ok(recovered);
                }
                let has_more_attempts = attempt + 1 < max_attempts;
                if !has_more_attempts {
                    return Err(error);
                }
                tokio::time::sleep(Duration::from_millis(auth.connect_retry_delay_ms)).await;
            }
        }
    }
    unreachable!("post_process_start_with_recovery exhausted attempts without returning")
}

pub(crate) fn build_http_client(auth: &ResolvedClientAuth) -> Result<Client> {
    build_http_client_from_tls(
        auth.request_timeout_seconds,
        auth.socks5_hostname.as_deref(),
        auth.tls_ca_cert.as_ref(),
        auth.tls_insecure_skip_verify,
        &auth.header,
    )
}

pub(crate) fn build_http_client_from_tls(
    request_timeout_seconds: u64,
    socks5_hostname: Option<&str>,
    tls_ca_cert: Option<&PathBuf>,
    tls_insecure_skip_verify: bool,
    extra_headers: &[String],
) -> Result<Client> {
    let connect_timeout_seconds = request_timeout_seconds.min(DEFAULT_CONNECT_TIMEOUT_SECONDS);
    let mut builder = ClientBuilder::new()
        .timeout(Duration::from_secs(request_timeout_seconds))
        .connect_timeout(Duration::from_secs(connect_timeout_seconds))
        .http1_only()
        .tcp_nodelay(true);
    if let Some(proxy) = socks5_hostname {
        builder = builder.proxy(Proxy::all(normalize_socks5_proxy_url(proxy)?)?);
    }
    if tls_insecure_skip_verify {
        builder = builder.danger_accept_invalid_certs(true);
    }
    if let Some(path) = tls_ca_cert {
        let cert = std::fs::read(path)
            .with_context(|| format!("failed to read tls ca cert {}", path.display()))?;
        let cert = Certificate::from_pem(&cert)
            .with_context(|| format!("failed to parse tls ca cert {}", path.display()))?;
        builder = builder.add_root_certificate(cert);
    }
    if !extra_headers.is_empty() {
        builder = builder.default_headers(parse_extra_headers(extra_headers)?);
    }
    Ok(builder.build()?)
}

fn normalize_socks5_proxy_url(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("--socks5-hostname must not be empty"));
    }
    if trimmed.contains("://") {
        return Ok(trimmed.to_string());
    }
    Ok(format!("socks5h://{trimmed}"))
}

pub(crate) async fn send_with_retry<F>(
    server: &str,
    socks5_hostname: Option<&str>,
    connect_retries: usize,
    retry_delay_ms: u64,
    retry_safe: bool,
    mut build_request: F,
) -> Result<reqwest::Response>
where
    F: FnMut() -> Result<reqwest::RequestBuilder>,
{
    let max_attempts = if retry_safe {
        connect_retries.saturating_add(1)
    } else {
        1
    };

    for attempt in 0..max_attempts {
        let request = build_request()?;
        match request.send().await {
            Ok(response) => {
                let can_retry = retry_safe
                    && attempt + 1 < max_attempts
                    && should_retry_response_status(response.status());
                if can_retry {
                    tokio::time::sleep(Duration::from_millis(retry_delay_ms)).await;
                    continue;
                }
                return Ok(response);
            }
            Err(error) => {
                let can_retry =
                    retry_safe && attempt + 1 < max_attempts && should_retry_request_error(&error);
                if can_retry {
                    tokio::time::sleep(Duration::from_millis(retry_delay_ms)).await;
                    continue;
                }
                return Err(wrap_request_error(error, server, socks5_hostname));
            }
        }
    }

    unreachable!("send_with_retry exhausted attempts without returning")
}

fn should_retry_request_error(error: &reqwest::Error) -> bool {
    error.is_connect() || error.is_timeout()
}

fn is_retryable_process_start_transport_error(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}").to_ascii_lowercase();
    text.contains("connection closed before message completed")
        || text.contains("client error (sendrequest)")
        || text.contains("error sending request for url")
}

fn should_retry_response_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT
            | StatusCode::TOO_MANY_REQUESTS
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

pub(crate) fn parse_extra_headers(extra_headers: &[String]) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    for raw in extra_headers {
        let (name, value) = raw
            .split_once(':')
            .ok_or_else(|| anyhow!("invalid --header {:?}; expected 'Name: value'", raw))?;
        let name = name.trim();
        let value = value.trim();
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .with_context(|| format!("invalid header name in --header {:?}", raw))?;
        let header_value = HeaderValue::from_str(value)
            .with_context(|| format!("invalid header value in --header {:?}", raw))?;
        headers.append(header_name, header_value);
    }
    Ok(headers)
}

pub(crate) async fn print_error_response(response: reqwest::Response) -> Result<ExitCode> {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    emit_error_body(status, &text)?;
    Ok(ExitCode::from(1))
}

/// Print a non-OK response body to stderr in the standard shape. Extracted from
/// `print_error_response` so callers that want to add an actionable hint can
/// reuse the body formatting after reading the response themselves.
pub(crate) fn emit_error_body(status: StatusCode, text: &str) -> Result<()> {
    if let Ok(body) = serde_json::from_str::<SimpleResponse>(text) {
        eprintln!("{}", serde_json::to_string(&body)?);
    } else {
        let mut message = format!(
            "failed to decode error response (status {}): {}",
            status, text
        );
        if text.contains("<html") || text.contains("proxy") || text.contains("gateway") {
            message.push_str(" ; hint: check http_proxy/https_proxy or proxy interception");
        }
        eprintln!("{}", message);
    }
    Ok(())
}

pub(crate) fn wrap_request_error(
    error: reqwest::Error,
    server: &str,
    socks5_hostname: Option<&str>,
) -> anyhow::Error {
    let detail = format_error_chain(&error);
    let socks_hint = socks5_hostname
        .map(|proxy| {
            format!(
                " ; SOCKS proxy {proxy} is configured for this profile: verify it is listening and reachable"
            )
        })
        .unwrap_or_default();
    if error.is_timeout() {
        anyhow!(
            "request timed out for {server}; check http_proxy/https_proxy or network reachability: {detail}{socks_hint}"
        )
    } else if error.is_connect() {
        anyhow!("failed to connect to {server}; check network reachability: {detail}{socks_hint}")
    } else {
        anyhow!("{detail}")
    }
}

async fn try_recover_existing_process_start(
    auth: &ResolvedClientAuth,
    payload: &ProcessStartRequest,
) -> Result<Option<ProcessStartResponse>> {
    let fetched = fetch_process_info(auth, &payload.process_id).await?;
    let Some(process) = fetched else {
        return Ok(None);
    };
    if process_matches_start_request(&process.process, payload) {
        return Ok(Some(ProcessStartResponse {
            ok: true,
            process_id: payload.process_id.clone(),
            created: false,
            already_exists: true,
        }));
    }
    Ok(None)
}

async fn fetch_process_info(
    auth: &ResolvedClientAuth,
    process_id: &str,
) -> Result<Option<ProcessGetResponse>> {
    let payload = ProcessGetRequest {
        process_id: process_id.to_string(),
    };
    let response = post_json(auth, "/v1/process/get", &payload, true).await?;
    if response.status() == StatusCode::OK {
        let body: ProcessGetResponse = response.json().await?;
        return Ok(Some(body));
    }
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if status == StatusCode::BAD_REQUEST && text.contains("unknown process_id") {
        return Ok(None);
    }
    Err(anyhow!(
        "failed to probe process state for {} after start transport error (status {}): {}",
        process_id,
        status,
        text
    ))
}

fn process_matches_start_request(
    process: &crate::protocol::ProcessInfo,
    payload: &ProcessStartRequest,
) -> bool {
    let requested_cwd = match &payload.cwd {
        Some(cwd) if std::path::Path::new(cwd).is_absolute() => cwd.clone(),
        Some(cwd) => {
            let mut path = std::path::PathBuf::from(&payload.remote_root);
            path.push(cwd);
            path.display().to_string()
        }
        None => payload.remote_root.clone(),
    };
    let requested_output_limit = payload
        .output_bytes_limit
        .unwrap_or(process.output_bytes_limit);
    process.process_id == payload.process_id
        && process.remote_root == payload.remote_root
        && process.cwd == requested_cwd
        && process.command == payload.command
        && process.pipe_stdin == payload.pipe_stdin
        && (process.kill_tree_on_terminate == payload.kill_tree_on_terminate
            || (!payload.kill_tree_on_terminate && process.kill_tree_on_terminate))
        && process.timeout_seconds == payload.timeout_seconds
        && process.output_bytes_limit == requested_output_limit
}

pub(crate) async fn process_error_response(response: reqwest::Response) -> anyhow::Error {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if let Ok(body) = serde_json::from_str::<SimpleResponse>(&text) {
        if let Some(error) = body.error {
            anyhow!("request failed with status {}: {}", status, error)
        } else {
            anyhow!("request failed with status {}", status)
        }
    } else {
        anyhow!("request failed with status {}: {}", status, text)
    }
}

fn format_error_chain(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut current = error.source();
    let mut causes = Vec::new();
    while let Some(source) = current {
        causes.push(source.to_string());
        current = source.source();
    }
    if !causes.is_empty() {
        message.push_str(" ; caused by: ");
        message.push_str(&causes.join(" -> "));
    }
    message
}

#[cfg(test)]
mod tests {
    use super::normalize_socks5_proxy_url;
    use anyhow::Result;

    #[test]
    fn normalize_socks5_proxy_url_adds_remote_dns_scheme() -> Result<()> {
        assert_eq!(
            normalize_socks5_proxy_url("127.0.0.1:1086")?,
            "socks5h://127.0.0.1:1086"
        );
        assert_eq!(
            normalize_socks5_proxy_url("socks5h://127.0.0.1:1086")?,
            "socks5h://127.0.0.1:1086"
        );
        Ok(())
    }
}

pub(crate) fn normalize_server_url(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}
