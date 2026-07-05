use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};

use crate::protocol::{AgentLease, AgentMode, LeaseStatus};

#[derive(Debug, Default)]
pub struct ModeRegistry {
    current_mode: AgentMode,
    leases: BTreeMap<(String, String), AgentLease>,
}

impl ModeRegistry {
    pub fn current_mode(&self) -> AgentMode {
        self.current_mode.clone()
    }

    pub fn leases(&self) -> Vec<AgentLease> {
        self.leases.values().cloned().collect()
    }

    pub fn expire_stale_leases(&mut self) {
        let now = unix_now_ms();
        for lease in self.leases.values_mut() {
            if lease.status != LeaseStatus::Active {
                continue;
            }
            let ttl_ms = u128::from(lease.ttl_seconds).saturating_mul(1000);
            if ttl_ms == 0 || now.saturating_sub(lease.last_heartbeat_at_unix_ms) > ttl_ms {
                lease.status = LeaseStatus::Expired;
                lease.released_at_unix_ms = Some(now);
            }
        }
    }

    pub fn switch_mode(
        &mut self,
        mode: AgentMode,
        task_id: Option<String>,
        lease_id: Option<String>,
        ttl_seconds: Option<u64>,
        heartbeat_seconds: Option<u64>,
        max_renewals: Option<u32>,
    ) -> Result<Option<AgentLease>> {
        self.current_mode = mode.clone();
        match mode {
            AgentMode::Single => Ok(None),
            AgentMode::Shared => {
                let task_id =
                    task_id.ok_or_else(|| anyhow::anyhow!("task_id is required in shared mode"))?;
                let lease_id = lease_id.unwrap_or_else(|| format!("{task_id}-lease"));
                let now = unix_now_ms();
                let lease = AgentLease {
                    task_id: task_id.clone(),
                    lease_id: lease_id.clone(),
                    mode,
                    status: LeaseStatus::Active,
                    ttl_seconds: ttl_seconds.unwrap_or(300),
                    heartbeat_seconds: heartbeat_seconds.unwrap_or(30),
                    max_renewals: max_renewals.unwrap_or(20),
                    renewals: 0,
                    acquired_at_unix_ms: now,
                    last_heartbeat_at_unix_ms: now,
                    released_at_unix_ms: None,
                };
                self.leases.insert((task_id, lease_id), lease.clone());
                Ok(Some(lease))
            }
        }
    }

    pub fn renew(&mut self, task_id: &str, lease_id: &str) -> Result<AgentLease> {
        self.expire_stale_leases();
        let lease = self
            .leases
            .get_mut(&(task_id.to_string(), lease_id.to_string()))
            .ok_or_else(|| anyhow::anyhow!("unknown lease: {task_id}/{lease_id}"))?;
        if lease.status != LeaseStatus::Active {
            bail!("lease is not active: {task_id}/{lease_id}");
        }
        lease.renewals = lease.renewals.saturating_add(1);
        if lease.renewals > lease.max_renewals {
            lease.status = LeaseStatus::Expired;
            lease.released_at_unix_ms = Some(unix_now_ms());
            bail!("lease expired: {task_id}/{lease_id}");
        }
        lease.last_heartbeat_at_unix_ms = unix_now_ms();
        Ok(lease.clone())
    }

    pub fn validate_active_lease(&mut self, task_id: &str, lease_id: &str) -> Result<AgentLease> {
        self.expire_stale_leases();
        let lease = self
            .leases
            .get(&(task_id.to_string(), lease_id.to_string()))
            .ok_or_else(|| anyhow::anyhow!("unknown lease: {task_id}/{lease_id}"))?;
        if lease.status != LeaseStatus::Active {
            bail!("lease is not active: {task_id}/{lease_id}");
        }
        Ok(lease.clone())
    }

    pub fn release(&mut self, task_id: &str, lease_id: &str) -> Result<AgentLease> {
        self.expire_stale_leases();
        let lease = self
            .leases
            .get_mut(&(task_id.to_string(), lease_id.to_string()))
            .ok_or_else(|| anyhow::anyhow!("unknown lease: {task_id}/{lease_id}"))?;
        lease.status = LeaseStatus::Released;
        lease.released_at_unix_ms = Some(unix_now_ms());
        let released = lease.clone();
        if !self.has_active_leases() {
            self.current_mode = AgentMode::Single;
        }
        Ok(released)
    }

    pub fn from_headers(headers: &axum::http::HeaderMap) -> Option<(String, String, String)> {
        let mode = headers
            .get(crate::protocol::HEADER_AGENT_MODE)?
            .to_str()
            .ok()?
            .to_string();
        let task_id = headers
            .get(crate::protocol::HEADER_TASK_ID)?
            .to_str()
            .ok()?
            .to_string();
        let lease_id = headers
            .get(crate::protocol::HEADER_LEASE_ID)?
            .to_str()
            .ok()?
            .to_string();
        Some((mode, task_id, lease_id))
    }
}

impl ModeRegistry {
    fn has_active_leases(&self) -> bool {
        self.leases
            .values()
            .any(|lease| lease.status == LeaseStatus::Active)
    }
}

fn unix_now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
