use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SimpleResponse {
    pub ok: bool,
    pub error: Option<String>,
}
