use anyhow::{Result, anyhow, bail};
use axum::http::{HeaderMap, header};

use super::ServerState;
use crate::mode::ModeRegistry;
use crate::protocol::AgentMode;

pub(super) fn authorized(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("Bearer {token}"))
}

pub(super) async fn validate_execution_lease(
    state: &ServerState,
    headers: &HeaderMap,
) -> Result<()> {
    let mut registry = state.modes.lock().await;
    registry.expire_stale_leases();
    if registry.current_mode() != AgentMode::Shared {
        return Ok(());
    }
    let (mode, task_id, lease_id) = ModeRegistry::from_headers(headers).ok_or_else(|| {
        anyhow!(
            "shared mode requires lease headers: {}, {}, {}",
            crate::protocol::HEADER_AGENT_MODE,
            crate::protocol::HEADER_TASK_ID,
            crate::protocol::HEADER_LEASE_ID
        )
    })?;
    if !mode.eq_ignore_ascii_case("shared") {
        bail!("lease header mode must be shared");
    }
    registry.validate_active_lease(&task_id, &lease_id)?;
    Ok(())
}
