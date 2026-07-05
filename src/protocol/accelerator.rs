use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AcceleratorKind {
    Gpu,
    Npu,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcceleratorStatusRequest {
    pub kind: AcceleratorKind,
    #[serde(default)]
    pub gpus: Option<String>,
    #[serde(default)]
    pub process_match: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcceleratorStatusResponse {
    pub ok: bool,
    pub kind: AcceleratorKind,
    pub available: bool,
    pub provider: Option<String>,
    pub reason: Option<String>,
    pub devices: Vec<AcceleratorDevice>,
    pub processes: Vec<AcceleratorProcess>,
    pub agent_hint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcceleratorDevice {
    pub index: u32,
    pub name: String,
    pub uuid: Option<String>,
    pub memory_used_mib: Option<u64>,
    pub memory_total_mib: Option<u64>,
    pub utilization_percent: Option<u32>,
    pub pstate: Option<String>,
    pub power_draw_milliwatts: Option<u64>,
    pub temperature_celsius: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcceleratorProcess {
    pub pid: i32,
    pub ppid: Option<i32>,
    pub process_group_id: Option<i32>,
    pub session_id: Option<i32>,
    pub elapsed: Option<String>,
    pub stat: Option<String>,
    pub user: Option<String>,
    pub command: Option<String>,
    pub gpu_index: Option<u32>,
    pub gpu_uuid: Option<String>,
    pub used_memory_mib: Option<u64>,
}
