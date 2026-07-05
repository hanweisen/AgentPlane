use serde::{Deserialize, Serialize};

pub const HEADER_AGENT_MODE: &str = "x-agentplane-agent-mode";
pub const HEADER_TASK_ID: &str = "x-agentplane-task-id";
pub const HEADER_LEASE_ID: &str = "x-agentplane-lease-id";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentMode {
    #[default]
    Single,
    Shared,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LeaseStatus {
    Active,
    Released,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentLease {
    pub task_id: String,
    pub lease_id: String,
    pub mode: AgentMode,
    pub status: LeaseStatus,
    pub ttl_seconds: u64,
    pub heartbeat_seconds: u64,
    pub max_renewals: u32,
    pub renewals: u32,
    pub acquired_at_unix_ms: u128,
    pub last_heartbeat_at_unix_ms: u128,
    pub released_at_unix_ms: Option<u128>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModeGetRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModeGetResponse {
    pub ok: bool,
    pub current_mode: AgentMode,
    pub leases: Vec<AgentLease>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModeSwitchRequest {
    pub mode: AgentMode,
    pub task_id: Option<String>,
    pub lease_id: Option<String>,
    pub ttl_seconds: Option<u64>,
    pub heartbeat_seconds: Option<u64>,
    pub max_renewals: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModeSwitchResponse {
    pub ok: bool,
    pub current_mode: AgentMode,
    pub lease: Option<AgentLease>,
    pub leases: Vec<AgentLease>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaseRenewRequest {
    pub task_id: String,
    pub lease_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaseRenewResponse {
    pub ok: bool,
    pub lease: AgentLease,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaseReleaseRequest {
    pub task_id: String,
    pub lease_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaseReleaseResponse {
    pub ok: bool,
    pub lease: AgentLease,
}
