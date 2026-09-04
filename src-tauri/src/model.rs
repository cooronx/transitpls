use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub metadata: DocumentMetadata,
    pub chapters: Vec<Chapter>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentMetadata {
    pub title: String,
    pub source_language: String,
    pub target_language: String,
    pub source_format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    pub id: String,
    pub title: String,
    pub status: ItemStatus,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ItemStatus {
    Pending,
    Translated,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub id: String,
    pub ordinal: usize,
    pub source: String,
    pub target: Option<String>,
    pub kind: SegmentKind,
    pub status: ItemStatus,
    pub source_hash: String,
    #[serde(default)]
    pub meta: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SegmentKind {
    Paragraph,
    Heading,
    Quote,
    Metadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectState {
    pub id: String,
    pub title: String,
    pub source_file: String,
    pub source_path: String,
    pub source_hash: String,
    pub source_language: String,
    pub target_language: String,
    pub status: ProjectStatus,
    pub chapters_total: usize,
    pub chapters_completed: usize,
    pub created_at: String,
    pub updated_at: String,
    pub max_segment_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProjectStatus {
    Initialized,
    Translating,
    Translated,
    Failed,
}
