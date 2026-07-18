use serde::{Deserialize, Serialize};

pub const HEADER_UPLOAD_CHUNK_SHA256: &str = "x-agentplane-upload-chunk-sha256";
pub const HEADER_UPLOAD_SYNC_SESSION_ID: &str = "x-agentplane-upload-sync-session-id";
pub const HEADER_UPLOAD_LOCK_TOKEN: &str = "x-agentplane-upload-lock-token";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileWrite {
    pub path: String,
    pub content_b64: String,
    pub executable: bool,
    #[serde(default)]
    pub mode: Option<u32>,
    #[serde(default)]
    pub checksum_sha256: Option<String>,
    #[serde(default)]
    pub preuploaded: bool,
    #[serde(default)]
    pub preupload_existed: bool,
    #[serde(default)]
    pub preupload_skipped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileReadRequest {
    pub remote_root: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileReadResponse {
    pub ok: bool,
    pub path: String,
    pub content_b64: String,
    pub executable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileStatRequest {
    pub remote_root: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileStatResponse {
    pub ok: bool,
    pub path: String,
    pub exists: bool,
    pub file_type: String,
    pub size: Option<u64>,
    pub modified_unix_ms: Option<u128>,
    pub executable: bool,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileWriteRequest {
    pub remote_root: String,
    pub path: String,
    pub content_b64: String,
    pub executable: bool,
    #[serde(default = "default_true")]
    pub create_parents: bool,
    #[serde(default)]
    pub atomic: bool,
    #[serde(default)]
    pub mode: Option<u32>,
    #[serde(default)]
    pub preserve_mode: bool,
    #[serde(default)]
    pub checksum_sha256: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileUploadInitRequest {
    pub remote_root: String,
    pub path: String,
    pub total_size: u64,
    pub chunk_size: u64,
    pub executable: bool,
    #[serde(default = "default_true")]
    pub create_parents: bool,
    #[serde(default)]
    pub atomic: bool,
    #[serde(default)]
    pub mode: Option<u32>,
    #[serde(default)]
    pub preserve_mode: bool,
    pub checksum_sha256: String,
    #[serde(default)]
    pub resume: bool,
    #[serde(default)]
    pub sync_session_id: Option<String>,
    #[serde(default)]
    pub lock_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileUploadInitResponse {
    pub ok: bool,
    pub upload_id: String,
    pub received_bytes: u64,
    pub total_size: u64,
    pub chunk_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileUploadChunkRequest {
    pub upload_id: String,
    pub offset: u64,
    pub data_b64: String,
    #[serde(default)]
    pub chunk_checksum_sha256: Option<String>,
    #[serde(default)]
    pub sync_session_id: Option<String>,
    #[serde(default)]
    pub lock_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileUploadChunkResponse {
    pub ok: bool,
    pub upload_id: String,
    pub received_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileUploadStatusRequest {
    pub upload_id: String,
    #[serde(default)]
    pub sync_session_id: Option<String>,
    #[serde(default)]
    pub lock_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileUploadStatusResponse {
    pub ok: bool,
    pub upload_id: String,
    pub received_bytes: u64,
    pub total_size: u64,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileUploadFinishRequest {
    pub upload_id: String,
    #[serde(default)]
    pub sync_session_id: Option<String>,
    #[serde(default)]
    pub lock_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileUploadAbortRequest {
    pub upload_id: String,
    #[serde(default)]
    pub sync_session_id: Option<String>,
    #[serde(default)]
    pub lock_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileDeleteRequest {
    pub remote_root: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileFindRequest {
    pub remote_root: String,
    pub pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileFindResponse {
    pub ok: bool,
    pub matches: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileListRequest {
    pub remote_root: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileListResponse {
    pub ok: bool,
    pub entries: Vec<FileListEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileListEntry {
    pub path: String,
    pub is_dir: bool,
}
