use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileWrite {
    pub path: String,
    pub content_b64: String,
    pub executable: bool,
    #[serde(default)]
    pub mode: Option<u32>,
    #[serde(default)]
    pub checksum_sha256: Option<String>,
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
