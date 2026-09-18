//! 术语抽取：调用模型从原文/译文中提取术语，并对空响应做降级处理。

use super::matching::is_untranslated_source;
use super::sqlite::validate_term;
use super::{Term, TermPolicy, TermStatus};
use crate::llm::{TranslationClient, EMPTY_COMPLETION_ERROR};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ExtractionResponse {
    terms: Vec<ExtractedTerm>,
}

/// 抽取提示词与响应 schema 共用的固定类型词表；数据库中保存同样的字符串。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
enum TermType {
    Person,
    Place,
    Organization,
    Term,
    Appellation,
    Speech,
    FixedExpr,
}

impl TermType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Place => "place",
            Self::Organization => "organization",
            Self::Term => "term",
            Self::Appellation => "appellation",
            Self::Speech => "speech",
            Self::FixedExpr => "fixed_expr",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
struct ExtractedTerm {
    source: String,
    target: String,
    reading: Option<String>,
    #[serde(rename = "type")]
    term_type: TermType,
    gender: Option<String>,
    #[serde(default)]
    aliases: Vec<String>,
    note: Option<String>,
}

impl ExtractedTerm {
    fn into_term(self, chapter: usize) -> Term {
        Term {
            source: self.source,
            target: self.target,
            reading: self.reading,
            term_type: self.term_type.as_str().to_string(),
            gender: self.gender,
            aliases: self.aliases,
            first_chapter: chapter,
            note: self.note,
            status: TermStatus::Ok,
            policy: TermPolicy::Automatic,
            manual_target: None,
        }
    }
}

/// 从一段原文/译文中抽取术语，失败时按 `max_retries` 退避重试。
///
/// `known_terms` 是当前批次命中的既有术语，模型应沿用其译名而不是另造一个；
/// 返回空补全会直接报错，交由 `extract_terms_resilient` 决定是否拆分批次。
pub async fn extract_terms<C: TranslationClient + ?Sized>(
    client: &C,
    source_text: &str,
    target_text: &str,
    known_terms: &[Term],
    chapter: usize,
    max_retries: usize,
) -> Result<Vec<Term>, ExtractionError> {
    let known = known_terms
        .iter()
        .map(|term| {
            serde_json::json!({
                "source": term.source,
                "target": term.target,
                "aliases": term.aliases,
            })
        })
        .collect::<Vec<_>>();
    let user = serde_json::json!({
        "source": source_text,
        "target": target_text,
        "known_terms": known,
    })
    .to_string();
    let system = "TASK:TERM_EXTRACTION Extract names, places, organizations, domain terms, forms of address, speech habits, and fixed expressions whose translations should stay consistent. known_terms lists the existing glossary; whenever a known source or alias appears, reuse its target exactly and never propose another translation for it. Return only JSON as {\"terms\":[{\"source\":\"...\",\"target\":\"...\",\"reading\":null,\"type\":\"person\",\"gender\":null,\"aliases\":[],\"note\":null}]}. The type value must be exactly one of these literals: person, place, organization, term, appellation, speech, fixed_expr. For example, use term rather than domain term and person rather than name. Every field is required; use null for absent reading, gender, and note. Return an empty array when nothing qualifies.";
    let schema = crate::schema::response_schema::<ExtractionResponse>();
    let mut last_error = None;
    for attempt in 0..=max_retries {
        match client
            .complete_attempt(system, &user, attempt, Some(schema.clone()))
            .await
        {
            Ok(output) if output.text.trim().is_empty() => {
                return Err(ExtractionError::Response(
                    EMPTY_COMPLETION_ERROR.to_string(),
                ));
            }
            Ok(output) => match crate::llm::parse_json_response::<ExtractionResponse>(&output.text)
            {
                Ok(response) => {
                    let terms = response
                        .terms
                        .into_iter()
                        .map(|term| term.into_term(chapter))
                        .filter(|term| !is_untranslated_source(&term.source, &term.target))
                        .collect::<Vec<_>>();
                    match terms.iter().try_for_each(validate_term) {
                        Ok(()) => return Ok(terms),
                        Err(error) => last_error = Some(ExtractionError::Response(error)),
                    }
                }
                Err(error) => {
                    last_error = Some(ExtractionError::Response(format!(
                        "invalid term extraction JSON: {error}"
                    )));
                }
            },
            Err(error) if is_empty_completion_error(&error) => {
                return Err(ExtractionError::Response(error));
            }
            Err(error) => last_error = Some(ExtractionError::Request(error)),
        }
        if attempt < max_retries {
            tokio::time::sleep(Duration::from_secs(1_u64 << attempt.min(6))).await;
        }
    }
    Err(last_error
        .unwrap_or_else(|| ExtractionError::Response("no extraction attempt was made".to_string()))
        .with_retries(max_retries))
}

/// A failed term extraction, split by whether retrying could help.
#[derive(Debug)]
pub enum ExtractionError {
    /// The model answered, but the payload was empty or unusable; callers can
    /// retry the batch in smaller pieces or skip it without losing translation
    /// progress.
    Response(String),
    /// The request itself failed; the caller should surface the error.
    Request(String),
}

impl ExtractionError {
    fn with_retries(self, max_retries: usize) -> Self {
        let message = format!(
            "term extraction failed after {max_retries} retries: {}",
            self.message()
        );
        match self {
            Self::Response(_) => Self::Response(message),
            Self::Request(_) => Self::Request(message),
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::Response(message) | Self::Request(message) => message,
        }
    }
}

/// 带降级的抽取：整批返回空补全或无效响应时按段落对半拆分重试，单段仍失败则记录并跳过。
pub async fn extract_terms_resilient<C: TranslationClient + ?Sized>(
    client: &C,
    source_text: &str,
    target_text: &str,
    known_terms: &[Term],
    chapter: usize,
    max_retries: usize,
) -> Result<Vec<Term>, String> {
    let mut batches = vec![paragraph_pairs(source_text, target_text)];
    let mut merged = Vec::new();

    while let Some(batch) = batches.pop() {
        let source = batch
            .iter()
            .map(|(source, _)| source.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let target = batch
            .iter()
            .map(|(_, target)| target.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        match extract_terms(client, &source, &target, known_terms, chapter, max_retries).await {
            Ok(terms) => merged.extend(terms),
            Err(ExtractionError::Response(_)) if batch.len() > 1 => {
                let midpoint = batch.len() / 2;
                let mut left = batch;
                let right = left.split_off(midpoint);
                batches.push(right);
                batches.push(left);
            }
            Err(ExtractionError::Response(error)) => {
                let _ = client.record_failure(
                    "term_extraction",
                    serde_json::json!({
                        "kind": "term_extraction_skipped",
                        "error": error,
                        "chapter": chapter,
                        "source_chars": source.chars().count(),
                        "target_chars": target.chars().count(),
                    }),
                );
            }
            Err(ExtractionError::Request(error)) => return Err(error),
        }
    }

    let mut seen = HashSet::new();
    merged.retain(|term| {
        seen.insert((
            super::matching::normalize(&term.source),
            super::matching::normalize(&term.target),
        ))
    });
    Ok(merged)
}

/// 当原文与译文的行数一致时按行配对，否则整段作为一个批次。
fn paragraph_pairs(source_text: &str, target_text: &str) -> Vec<(String, String)> {
    let sources = source_text.split('\n').collect::<Vec<_>>();
    let targets = target_text.split('\n').collect::<Vec<_>>();
    if sources.len() == targets.len() && sources.len() > 1 {
        sources
            .into_iter()
            .zip(targets)
            .map(|(source, target)| (source.to_string(), target.to_string()))
            .collect()
    } else {
        vec![(source_text.to_string(), target_text.to_string())]
    }
}

fn is_empty_completion_error(error: &str) -> bool {
    error.contains(EMPTY_COMPLETION_ERROR)
}
