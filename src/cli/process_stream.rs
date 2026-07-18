use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::cli_client::parse_extra_headers;
use crate::config::ResolvedClientAuth;
use crate::protocol::{ProcessEventMessage, ProcessEventSubscribeRequest, ProcessReadResponse};

pub(super) struct ProcessEventClient {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    read_timeout: Duration,
}

impl ProcessEventClient {
    pub(super) async fn connect(
        auth: &ResolvedClientAuth,
        subscription: &ProcessEventSubscribeRequest,
    ) -> Result<Self> {
        ensure_websocket_compatible(auth)?;
        let url = websocket_url(&auth.server)?;
        let mut request = url
            .as_str()
            .into_client_request()
            .context("failed to create WebSocket upgrade request")?;
        let extra_headers = parse_extra_headers(&auth.header)?;
        for (name, value) in &extra_headers {
            request.headers_mut().append(name.clone(), value.clone());
        }
        request.headers_mut().insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", auth.token))
                .context("invalid bearer token header")?,
        );

        let (mut stream, _) = tokio::time::timeout(
            Duration::from_secs(auth.request_timeout_seconds.min(10)),
            connect_async(request),
        )
        .await
        .context("WebSocket connection timed out")?
        .context("WebSocket upgrade failed")?;
        stream
            .send(Message::Text(serde_json::to_string(subscription)?.into()))
            .await
            .context("failed to send process event subscription")?;
        Ok(Self {
            stream,
            read_timeout: Duration::from_secs(auth.request_timeout_seconds),
        })
    }

    pub(super) async fn next_read(&mut self) -> Result<ProcessReadResponse> {
        loop {
            let message = tokio::time::timeout(self.read_timeout, self.stream.next())
                .await
                .context("WebSocket process event read timed out")?
                .ok_or_else(|| anyhow!("WebSocket process event stream closed"))?
                .context("WebSocket process event read failed")?;
            match message {
                Message::Text(text) => match serde_json::from_str::<ProcessEventMessage>(&text)
                    .context("failed to decode process event")?
                {
                    ProcessEventMessage::Read { response } => return Ok(response),
                    ProcessEventMessage::Error { error } => bail!(error),
                },
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Close(frame) => {
                    bail!("WebSocket process event stream closed: {frame:?}")
                }
                Message::Binary(_) | Message::Frame(_) => {
                    bail!("unexpected binary WebSocket process event")
                }
            }
        }
    }
}

pub(super) fn ensure_websocket_compatible(auth: &ResolvedClientAuth) -> Result<()> {
    if auth.socks5_hostname.is_some() {
        bail!(
            "WebSocket transport does not support --socks5-hostname; set AP_PROCESS_TRANSPORT=http"
        )
    }
    if auth.tls_ca_cert.is_some() {
        bail!("WebSocket transport does not support --tls-ca-cert; set AP_PROCESS_TRANSPORT=http")
    }
    if auth.tls_insecure_skip_verify {
        bail!(
            "WebSocket transport does not support --tls-insecure-skip-verify; set AP_PROCESS_TRANSPORT=http"
        )
    }
    Ok(())
}

fn websocket_url(server: &str) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(server).context("invalid AgentPlane server URL")?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        other => bail!("unsupported AgentPlane server URL scheme: {other}"),
    };
    url.set_scheme(scheme)
        .map_err(|_| anyhow!("failed to set WebSocket URL scheme"))?;
    let path = format!("{}/v1/events", url.path().trim_end_matches('/'));
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::extract::ws::{Message as AxumMessage, WebSocketUpgrade};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::get;

    use crate::config::ResolvedClientAuth;
    use crate::protocol::{ProcessEventMessage, ProcessEventSubscribeRequest, ProcessReadResponse};

    use super::ProcessEventClient;
    use super::websocket_url;
    use anyhow::Result;

    #[test]
    fn websocket_url_preserves_gateway_prefix() -> Result<()> {
        assert_eq!(
            websocket_url("https://example.test/vscode/proxy/8765/")?.as_str(),
            "wss://example.test/vscode/proxy/8765/v1/events"
        );
        assert_eq!(
            websocket_url("http://127.0.0.1:8765")?.as_str(),
            "ws://127.0.0.1:8765/v1/events"
        );
        Ok(())
    }

    #[tokio::test]
    async fn websocket_upgrade_forwards_auth_and_gateway_headers() -> Result<()> {
        async fn events(headers: HeaderMap, websocket: WebSocketUpgrade) -> impl IntoResponse {
            if headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                != Some("Bearer test-token")
                || headers
                    .get("x-agentplane-gateway")
                    .and_then(|value| value.to_str().ok())
                    != Some("ok")
            {
                return StatusCode::FORBIDDEN.into_response();
            }
            websocket
                .on_upgrade(|mut socket| async move {
                    let _ = socket.recv().await;
                    let event = ProcessEventMessage::Read {
                        response: ProcessReadResponse {
                            ok: true,
                            process_id: "header-test".to_string(),
                            chunks: Vec::new(),
                            next_seq: 0,
                            available_from_seq: 0,
                            cursor_expired: false,
                            exited: true,
                            exit_code: Some(0),
                            truncated: false,
                            failure: None,
                        },
                    };
                    let _ = socket
                        .send(AxumMessage::Text(
                            serde_json::to_string(&event)
                                .expect("serialize event")
                                .into(),
                        ))
                        .await;
                })
                .into_response()
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/v1/events", get(events))).await
        });
        let auth = ResolvedClientAuth {
            server: format!("http://{address}"),
            token: "test-token".to_string(),
            socks5_hostname: None,
            request_timeout_seconds: 5,
            connect_retries: 0,
            connect_retry_delay_ms: 0,
            tls_ca_cert: None,
            tls_insecure_skip_verify: false,
            header: vec!["X-AgentPlane-Gateway: ok".to_string()],
            agent_id: "test-agent".to_string(),
        };
        let mut client = ProcessEventClient::connect(
            &auth,
            &ProcessEventSubscribeRequest {
                process_id: "header-test".to_string(),
                after_seq: None,
                max_bytes: None,
            },
        )
        .await?;
        let response = client.next_read().await?;
        server.abort();

        assert!(response.exited);
        assert_eq!(response.exit_code, Some(0));
        Ok(())
    }
}
