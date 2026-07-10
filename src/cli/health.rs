use std::process::ExitCode;

use anyhow::{Result, anyhow};
use reqwest::StatusCode;
use reqwest::header::AUTHORIZATION;

use crate::cli_client::{
    build_http_client_from_tls, normalize_server_url, print_error_response, send_with_retry,
};
use crate::config::{
    ClientProfile, DEFAULT_CONNECT_RETRIES, DEFAULT_HTTP_TIMEOUT_SECONDS, RETRY_BASE_DELAY_MS,
};
use crate::protocol::SimpleResponse;

use super::HealthArgs;

pub(super) async fn health(args: HealthArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let server = args
        .server
        .clone()
        .or_else(|| profile.server.clone())
        .ok_or_else(|| anyhow!("--server is required unless AP_SERVER is set in --profile"))?;
    let mut headers = profile.headers.clone();
    headers.extend(args.header.clone());
    let token = args.token.clone();
    let response = send_with_retry(
        &server,
        args.connect_retries
            .or(profile.connect_retries)
            .unwrap_or(DEFAULT_CONNECT_RETRIES),
        args.connect_retry_delay_ms
            .or(profile.connect_retry_delay_ms)
            .unwrap_or(RETRY_BASE_DELAY_MS),
        true,
        || {
            let client = build_http_client_from_tls(
                args.request_timeout_seconds
                    .unwrap_or(DEFAULT_HTTP_TIMEOUT_SECONDS),
                args.socks5_hostname
                    .as_deref()
                    .or(profile.socks5_hostname.as_deref()),
                args.tls_ca_cert.as_ref(),
                args.tls_insecure_skip_verify,
                &headers,
            )?;
            let mut request = client.get(format!("{}/healthz", normalize_server_url(&server)));
            if let Some(token) = &token {
                request = request.header(AUTHORIZATION, format!("Bearer {token}"));
            }
            Ok(request)
        },
    )
    .await?;

    if response.status() == StatusCode::OK {
        let body: SimpleResponse = response.json().await?;
        // Merge client-side identity (server, optional label) into the printed
        // JSON without changing the wire SimpleResponse contract. `ok` stays the
        // source of truth for the exit code.
        let label = args.label.clone().or_else(|| profile.label.clone());
        let mut value = serde_json::to_value(&body)?;
        if let Some(obj) = value.as_object_mut() {
            obj.insert("server".to_string(), serde_json::json!(server));
            if let Some(label) = &label {
                obj.insert("label".to_string(), serde_json::json!(label));
            }
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(if body.ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        });
    }

    print_error_response(response).await
}
