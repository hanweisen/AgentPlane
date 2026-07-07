use serde::{Deserialize, Serialize};

use super::FileWrite;
use super::ResourceClaim;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    WorktreeDelta,
    RefSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncPayload {
    pub remote_root: String,
    pub writes: Vec<FileWrite>,
    pub deletes: Vec<String>,
    pub sync_mode: SyncMode,
    pub source_ref: Option<String>,
    pub preserve_paths: Vec<String>,
    pub command: Option<String>,
    pub timeout_seconds: u64,
    pub env: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default)]
    pub claims: Vec<ResourceClaim>,
    #[serde(default)]
    pub checksum: bool,
    #[serde(default)]
    pub preserve_mode: bool,
    #[serde(default)]
    pub atomic_files: bool,
    #[serde(default)]
    pub sync_session_id: Option<String>,
    #[serde(default)]
    pub lock_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncResponse {
    pub ok: bool,
    pub remote_root: String,
    pub write_count: usize,
    pub delete_count: usize,
    #[serde(default)]
    pub report: SyncReport,
    pub source_ref: Option<String>,
    pub preserve_paths: Vec<String>,
    pub result: CommandResult,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SyncReport {
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub skipped: Vec<String>,
    pub deleted: Vec<String>,
    pub conflict: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncSessionInitRequest {
    pub remote_root: String,
    pub agent_id: String,
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
    #[serde(default)]
    pub lock_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncSessionInitResponse {
    pub ok: bool,
    pub sync_session_id: String,
    pub lock_token: String,
    pub agent_id: String,
    pub remote_root: String,
    pub lock_key: String,
    pub expires_unix_ms: u128,
    pub heartbeat_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncSessionStatusRequest {
    pub sync_session_id: String,
    pub lock_token: String,
    pub remote_root: String,
    pub agent_id: String,
    #[serde(default)]
    pub lock_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncSessionReleaseRequest {
    pub sync_session_id: String,
    pub lock_token: String,
}

pub(crate) fn relative_path_matches_preserve_path(
    relative: &str,
    preserve_paths: &[String],
) -> bool {
    preserve_paths.iter().any(|preserve| {
        relative == preserve
            || relative
                .strip_prefix(preserve)
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

#[cfg(test)]
mod tests {
    use super::relative_path_matches_preserve_path;

    #[test]
    fn relative_path_matches_preserve_path_respects_path_boundaries() {
        let preserve_paths = vec!["target".to_string()];

        assert!(relative_path_matches_preserve_path(
            "target",
            &preserve_paths
        ));
        assert!(relative_path_matches_preserve_path(
            "target/foo",
            &preserve_paths
        ));
        assert!(relative_path_matches_preserve_path(
            "target/foo/bar",
            &preserve_paths
        ));
        assert!(!relative_path_matches_preserve_path(
            "target-cache",
            &preserve_paths
        ));
        assert!(!relative_path_matches_preserve_path(
            "target2",
            &preserve_paths
        ));
    }
}
