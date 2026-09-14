//! llm 模块的单元测试：提示词内容、响应校验与语言检测。

use super::mock::mock_output;
use super::{
    build_prompts, detect_source_language, sample_language_texts, validate_language_response,
    validate_response, CompletionOutput, RecentTarget, TranslationClient, TranslationContext,
};
use crate::model::{Chapter, Document, DocumentMetadata, ItemStatus, Segment, SegmentKind};
use crate::terms::{Term, TermStatus};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

fn document_with_source(source: &str) -> Document {
    Document {
        metadata: DocumentMetadata {
            title: "Test".to_string(),
            source_language: "auto".to_string(),
            target_language: "zh-CN".to_string(),
            source_format: "txt".to_string(),
        },
        chapters: vec![Chapter {
            id: "chapter-1".to_string(),
            title: "Chapter 1".to_string(),
            target_title: None,
            status: ItemStatus::Pending,
            meta: serde_json::json!({}),
            segments: vec![Segment {
                id: "segment-1".to_string(),
                ordinal: 0,
                source: source.to_string(),
                target: None,
                target_before_polish: None,
                polish_status: None,
                kind: SegmentKind::Paragraph,
                status: ItemStatus::Pending,
                source_hash: "hash".to_string(),
                meta: serde_json::json!({}),
            }],
        }],
    }
}

#[test]
fn translation_prompt_includes_relevant_terms() {
    let segment = document_with_source("Alice arrived.").chapters[0].segments[0].clone();
    let term = Term {
        source: "Alice".to_string(),
        target: "爱丽丝".to_string(),
        reading: None,
        term_type: "person".to_string(),
        gender: None,
        aliases: Vec::new(),
        first_chapter: 0,
        note: None,
        status: TermStatus::Resolved,
        policy: crate::terms::TermPolicy::Fixed,
        manual_target: Some("爱丽丝".to_string()),
    };
    let terms = [term];
    let context = TranslationContext {
        style_guide: &["Keep the voice".to_string()],
        book_synopsis: Some("Book synopsis"),
        chapter_digest: Some("Chapter digest"),
        terms: &terms,
        recent_targets: &[RecentTarget {
            chapter_id: "chapter-0".to_string(),
            segment_id: "segment-0".to_string(),
            target: "最近译文".to_string(),
        }],
    };
    let (_, user) = build_prompts(&[segment], "en", "zh-CN", &context);
    let value: serde_json::Value =
        serde_json::from_str(&user).expect("prompt should be valid JSON");
    assert_eq!(value["terms"][0]["target"], "爱丽丝");
    assert_eq!(value["terms"][0]["status"], "resolved");
    assert_eq!(value["segments"][0]["number"], 1);
    let positions = [
        "\"style\"",
        "\"book_synopsis\"",
        "\"chapter_digest\"",
        "\"terms\"",
        "\"recent_targets\"",
        "\"segments\"",
    ]
    .map(|field| user.find(field).expect("prompt field should exist"));
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
}

struct SequenceClient {
    responses: Arc<Mutex<Vec<Result<String, String>>>>,
}

impl SequenceClient {
    fn new(responses: Vec<Result<String, String>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
        }
    }
}

#[async_trait]
impl TranslationClient for SequenceClient {
    async fn complete(
        &self,
        _system_prompt: &str,
        _user_prompt: &str,
    ) -> Result<CompletionOutput, String> {
        self.responses
            .lock()
            .expect("sequence client mutex")
            .remove(0)
            .map(mock_output)
    }
}

#[test]
fn validates_count_and_rejects_empty_translation() {
    let valid = r#"{"translations":["甲","乙"]}"#;
    assert_eq!(
        validate_response(valid, 2).expect("valid response"),
        vec!["甲", "乙"]
    );
    let empty = r#"{"translations":[" ","乙"]}"#;
    assert!(validate_response(empty, 2).is_err());
    let incomplete = r#"{"translations":["甲"]}"#;
    assert!(validate_response(incomplete, 2).is_err());
    let fenced = format!("```json\n{valid}\n```");
    assert_eq!(
        validate_response(&fenced, 2).expect("fenced JSON should be accepted"),
        vec!["甲", "乙"]
    );
    assert_eq!(
        validate_response(&format!("Result:\n{valid}"), 2).unwrap(),
        vec!["甲", "乙"]
    );
    assert!(validate_response("no JSON in this response", 2).is_err());
}

#[test]
fn samples_three_random_excerpts_with_unicode_character_limit() {
    let document = document_with_source(&"あ".repeat(2_500));
    let samples = sample_language_texts(&document).expect("samples");
    assert_eq!(samples.len(), 3);
    assert!(samples.iter().all(|sample| sample.chars().count() == 1_000));
}

#[tokio::test]
async fn accepts_language_only_when_all_three_samples_agree() {
    let client = SequenceClient::new(vec![
        Ok(r#"{"language":"ja"}"#.to_string()),
        Ok(r#"{"language":"ja"}"#.to_string()),
        Ok(r#"{"language":"JA"}"#.to_string()),
    ]);
    let document = document_with_source(&"日本語の文章です。".repeat(150));
    assert_eq!(
        detect_source_language(&client, &document, 0)
            .await
            .expect("language detection"),
        "ja"
    );
}

#[tokio::test]
async fn rejects_language_when_samples_disagree() {
    let client = SequenceClient::new(vec![
        Ok(r#"{"language":"ja"}"#.to_string()),
        Ok(r#"{"language":"en"}"#.to_string()),
        Ok(r#"{"language":"ja"}"#.to_string()),
    ]);
    let document = document_with_source(&"sample text ".repeat(200));
    let error = detect_source_language(&client, &document, 0)
        .await
        .expect_err("disagreement must fail");
    assert!(error.contains("samples disagree"));
}

#[test]
fn validates_strict_iso_language_response() {
    assert_eq!(
        validate_language_response(r#"{"language":" JA "}"#).expect("valid code"),
        "ja"
    );
    assert!(validate_language_response(r#"{"language":"jpn"}"#).is_err());
    assert!(validate_language_response(r#"{"language":"ja","extra":true}"#).is_err());
}
