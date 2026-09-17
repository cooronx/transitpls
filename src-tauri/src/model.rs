//! 项目核心数据模型。
//!
//! 这些结构会序列化到 `project.json`、章节文件并暴露给前端，属于持久化与接口契约，
//! 修改字段或序列化格式时必须兼容已有项目数据。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 解析完成、等待翻译的整本文档。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    /// 书名、语言等文档级信息。
    pub metadata: DocumentMetadata,
    /// 按阅读顺序排列的章节。
    pub chapters: Vec<Chapter>,
}

/// 文档级元信息，初始化项目时写入项目状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentMetadata {
    /// 书名，TXT 取文件名，EPUB 优先取 OPF 标题。
    pub title: String,
    /// 源语言代码，`auto` 表示稍后交给模型识别。
    pub source_language: String,
    /// 目标语言代码。
    pub target_language: String,
    /// 源文件格式，目前为 `txt` 或 `epub`。
    pub source_format: String,
}

/// 一章及其全部段落，对应项目 `chapters/` 目录下的一个 JSON 文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    /// 章节稳定标识，形如 `chapter-序号-标题哈希`，决定章节排序。
    pub id: String,
    /// 原文标题。
    pub title: String,
    /// 译文标题，未翻译时为 `None`。
    #[serde(default)]
    pub target_title: Option<String>,
    /// 章节状态，全部段落翻译完成后置为 `Translated`。
    pub status: ItemStatus,
    /// 章节级附加标记，如 `source_digest`、`terms_extracted`、`retranslation_error`。
    pub meta: Value,
    /// 按阅读顺序排列的段落。
    pub segments: Vec<Segment>,
}

/// 章节与段落的翻译状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ItemStatus {
    /// 尚未翻译。
    Pending,
    /// 已有可用译文。
    Translated,
    /// 翻译失败，可在后续任务中重试。
    Failed,
}

/// 段落的润色状态，仅在执行过润色后写入。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PolishStatus {
    /// 待润色。
    Pending,
    /// 润色成功，`target` 为润色后的译文。
    Succeeded,
    /// 润色失败，`target` 保留润色前的草稿。
    Failed,
}

/// 可独立翻译的最小文本单元。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    /// 段落稳定标识，由章节 ID、段落类型和原文哈希生成。
    pub id: String,
    /// 段落在章节内的顺序号。
    pub ordinal: usize,
    /// 原文。
    pub source: String,
    /// 当前译文，未翻译时为 `None`。
    pub target: Option<String>,
    /// 润色前的译文草稿，并行润色时作为稳定参考，避免批次互相影响。
    #[serde(default)]
    pub target_before_polish: Option<String>,
    /// 润色状态，未经历过润色流程时为 `None`。
    #[serde(default)]
    pub polish_status: Option<PolishStatus>,
    /// 段落类型，决定提示词和导出时的处理方式。
    pub kind: SegmentKind,
    /// 翻译状态。
    pub status: ItemStatus,
    /// 原文内容哈希，用于检测原文变化。
    pub source_hash: String,
    /// 段落级附加标记，如 `previous_target`、`retranslation_error`。
    #[serde(default)]
    pub meta: Value,
}

/// 段落类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SegmentKind {
    /// 正文段落。
    Paragraph,
    /// 章节内标题。
    Heading,
    /// 引用或特殊排版段落。
    Quote,
    /// 元信息段落，保留给旧项目与特殊结构，导出时按普通段落处理。
    Metadata,
}

/// 本地翻译项目的整体状态，持久化为 `project.json`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectState {
    /// 项目 ID，等于源文件内容的 SHA-256，同时作为项目目录名。
    pub id: String,
    /// 书名。
    pub title: String,
    /// 用户导入时传入的源文件路径，保持原样仅用于展示。
    pub source_file: String,
    /// 规范化后的源文件绝对路径，用于按路径回查项目。
    pub source_path: String,
    /// 源文件内容哈希，文件被修改后会视为另一个项目。
    pub source_hash: String,
    /// 源语言代码。
    pub source_language: String,
    /// 目标语言代码。
    pub target_language: String,
    /// 项目整体状态。
    pub status: ProjectStatus,
    /// 解析出的章节总数。
    pub chapters_total: usize,
    /// 全部段落均已翻译的章节数。
    pub chapters_completed: usize,
    /// 创建时间，UTC RFC 3339 毫秒精度。
    pub created_at: String,
    /// 最近一次更新时间，UTC RFC 3339 毫秒精度。
    pub updated_at: String,
    /// 单个段落的最大字符数，导入后固定。
    pub max_segment_chars: usize,
}

/// 项目整体状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProjectStatus {
    /// 已导入项目，尚未开始翻译。
    Initialized,
    /// 翻译进行中，或存在未完成章节。
    Translating,
    /// 全部章节翻译完成。
    Translated,
    /// 最近一次任务失败，保存进度后会恢复为进行中或已完成。
    Failed,
}
