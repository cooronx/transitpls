//! 术语库：术语数据结构、SQLite 持久化、LLM 抽取与文本匹配。
//!
//! 每个项目有独立的 `terms.db`：`terms` 保存术语本身，`term_rules` 保存人工策略，
//! `term_evidence`/`term_conflicts` 保存译名冲突证据，`pending_term_extractions`
//! 保存中断后待补做的抽取批次。

mod extract;
mod matching;
mod sqlite;
mod store;

#[cfg(test)]
mod tests;

pub use extract::{extract_terms, extract_terms_resilient};
pub use matching::{matches_text, relevant_terms};
pub use store::TermStore;

use serde::{Deserialize, Serialize};

/// 术语处理策略。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TermPolicy {
    /// 自动：正常参与翻译提示词，出现译名冲突时等待人工裁定。
    #[default]
    Automatic,
    /// 固定：译名已人工确认，后续翻译必须使用该译名。
    Fixed,
    /// 非固定：不注入提示词，也不记录冲突。
    NonFixed,
    /// 忽略：完全跳过该术语。
    Ignored,
}

impl TermPolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Fixed => "fixed",
            Self::NonFixed => "non_fixed",
            Self::Ignored => "ignored",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "automatic" => Ok(Self::Automatic),
            "fixed" => Ok(Self::Fixed),
            "non_fixed" => Ok(Self::NonFixed),
            "ignored" => Ok(Self::Ignored),
            _ => Err(format!("invalid term policy in database: {value}")),
        }
    }
}

/// 术语条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Term {
    /// 原文术语。
    pub source: String,
    /// 当前采用的译名。
    pub target: String,
    /// 读音或注音，可为空。
    pub reading: Option<String>,
    /// 术语类型，取值见抽取提示词中的固定词表。
    #[serde(rename = "type")]
    pub term_type: String,
    /// 性别标记，仅对人物称谓有意义。
    pub gender: Option<String>,
    /// 同一术语的其他写法。
    #[serde(default)]
    pub aliases: Vec<String>,
    /// 首次出现的章节序号。
    pub first_chapter: usize,
    /// 备注。
    pub note: Option<String>,
    /// 术语状态。
    pub status: TermStatus,
    /// 人工设置的策略。
    #[serde(default)]
    pub policy: TermPolicy,
    /// 人工固定的译名，仅当策略为 Fixed 时有值。
    #[serde(default)]
    pub manual_target: Option<String>,
}

/// 术语状态。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TermStatus {
    /// 正常，无未解决的译名冲突。
    Ok,
    /// 存在多个候选译名，等待人工裁定。
    Conflict,
    /// 已人工裁定。
    Resolved,
}

impl TermStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Conflict => "conflict",
            Self::Resolved => "resolved",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "ok" => Ok(Self::Ok),
            "conflict" => Ok(Self::Conflict),
            "resolved" => Ok(Self::Resolved),
            _ => Err(format!("invalid term status in database: {value}")),
        }
    }
}

impl std::fmt::Display for TermStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 某个术语在一次抽取中出现的候选译名。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TermCandidate {
    pub source: String,
    pub target: String,
    pub chapter: usize,
}

/// 候选译名对应的原文/译文片段，用于人工判定。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TermEvidence {
    pub chapter: usize,
    pub source_excerpt: String,
    pub target_excerpt: String,
}

/// 冲突中某个候选译名的汇总信息。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConflictCandidate {
    pub target: String,
    pub occurrences: usize,
    pub chapters: Vec<usize>,
    pub evidence: Vec<TermEvidence>,
}

/// 一个术语的完整冲突信息，供界面展示与裁定。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TermConflict {
    pub source: String,
    pub current_target: String,
    pub policy: TermPolicy,
    pub manual_target: Option<String>,
    pub unresolved_events: usize,
    pub resolved_events: usize,
    pub candidates: Vec<ConflictCandidate>,
}

/// 同一别名同时指向多个术语的冲突记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasConflict {
    pub alias: String,
    pub first_source: String,
    pub second_source: String,
}

/// 待补做的术语抽取任务，翻译中断后由下次运行继续处理。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingExtraction {
    pub chapter_id: String,
    pub batch_key: String,
    pub source_text: String,
    pub target_text: String,
}
