//! 全书分析：识别源语言，生成文风指南、人物与术语，并为各章生成摘要。
//!
//! 分析结果写入项目目录的 `analysis.json`，章节摘要写入章节 `meta`；
//! 已存在的分析结果会被复用，`force` 为 true 时重新生成。

use crate::llm::TranslationClient;
use crate::model::{Chapter, Document, DocumentMetadata, ProjectState};
use crate::state;
use crate::terms::{Term, TermStatus, TermStore};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

const BOOK_SAMPLE_CHARS: usize = 4_000;
const CHAPTER_SAMPLE_CHARS: usize = 12_000;

/// 开书分析提示词：`characters` 与 `terms` 采用与翻译后抽取相同的收录边界，
/// 普通词、普通场所、章节标题与一次性对白不进入术语库。
const BOOK_ANALYSIS_PROMPT: &str = concat!(
    "TASK:BOOK_STYLE_ANALYSIS Analyze the whole book's style. ",
    "Return only JSON with string fields genre, tone, narration, pacing, register, dialogue_style, and rhetoric; ",
    "style_guide must be an array of non-empty strings; characters and terms must be arrays; book_synopsis must be null. ",
    "characters and terms follow one inclusion rule: person names, proper place names, organization names, and concepts that carry a specific meaning in this work and need one consistent translation. ",
    "Add a form of address only when it is tied to a specific character or identity and needs one consistent translation. ",
    "Add a speech entry only for a catchphrase that a character repeatedly uses as a signature expression. ",
    "Do not add ordinary nouns, verbs, adjectives, generic place names such as a shopping mall, chapter titles, one-off dialogue lines, greetings, or temporary descriptions; ",
    "describe character tone, sentence-ending habits, and general speaking style in style_guide instead. ",
    "Empty arrays are fine when the samples contain no terminology. ",
    "Every character and term object must contain string source and target, nullable string reading and gender, string-array aliases, zero-based integer first_chapter, nullable string note, and type chosen from person, place, organization, term, appellation, speech, or fixed_expr.",
);

/// 全书分析结果，作为翻译与润色的共享上下文。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BookAnalysis {
    /// 题材。
    pub genre: String,
    /// 整体语气。
    pub tone: String,
    /// 文风指南，逐条列出翻译需要遵循的风格要求。
    pub style_guide: Vec<String>,
    /// 叙事视角与人称。
    pub narration: String,
    /// 节奏特点。
    pub pacing: String,
    /// 语体（书面/口语等）。
    pub register: String,
    /// 对话风格。
    pub dialogue_style: String,
    /// 修辞特点。
    pub rhetoric: String,
    /// 人物表，与术语一并写入术语库。
    pub characters: Vec<AnalysisTerm>,
    /// 其他专有名词与固定表达。
    pub terms: Vec<AnalysisTerm>,
    /// 全书梗概，仅完整分析模式下生成。
    pub book_synopsis: Option<String>,
}

/// 分析阶段抽取的人物或术语。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct AnalysisTerm {
    /// 原文。
    pub source: String,
    /// 建议译名。
    pub target: String,
    /// 读音或注音。
    pub reading: Option<String>,
    /// 类型，取值与术语抽取一致。
    #[serde(rename = "type")]
    pub term_type: String,
    /// 性别标记。
    pub gender: Option<String>,
    /// 别名写法。
    #[serde(default)]
    pub aliases: Vec<String>,
    /// 首次出现的章节序号。
    pub first_chapter: usize,
    /// 备注。
    pub note: Option<String>,
}

/// 章节摘要接口的响应。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DigestResponse {
    source_digest: String,
}

/// 全书梗概接口的响应。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SynopsisResponse {
    book_synopsis: String,
}

/// 运行（或复用）全书分析。
///
/// `full_book` 为 true 时逐章生成摘要并汇总梗概；`force` 为 true 时忽略已有结果。
pub async fn prepare<C: TranslationClient + ?Sized>(
    client: &C,
    state_dir: &Path,
    project: &mut ProjectState,
    chapters: &mut [Chapter],
    full_book: bool,
    force: bool,
    max_retries: usize,
) -> Result<BookAnalysis, String> {
    let project_dir = state::project_dir(state_dir, &project.id);
    let analysis_path = project_dir.join("analysis.json");
    let term_store = TermStore::open(project_dir.join("terms.db"))?;

    if project.source_language == "auto" {
        let document = document_from_state(project, chapters);
        project.source_language = crate::llm::detect_source_language(
            client,
            &document,
            max_retries,
        )
        .await
        .map_err(|error| {
            format!(
                "automatic source-language detection failed: {error}; rerun init with --source-language <code>"
            )
        })?;
        state::save_project(state_dir, project)?;
    }

    let mut analysis = if analysis_path.exists() && !force {
        state::read_json(&analysis_path)?
    } else {
        let samples = sample_book(chapters);
        let user = serde_json::json!({
            "source_language": project.source_language,
            "target_language": project.target_language,
            "samples": samples,
        })
        .to_string();
        let mut value: BookAnalysis =
            call_json(client, BOOK_ANALYSIS_PROMPT, &user, max_retries).await?;
        value.book_synopsis = None;
        validate_analysis(&value)?;
        state::write_json_atomic(&analysis_path, &value)?;
        value
    };

    for term in analysis.characters.iter().chain(&analysis.terms) {
        term_store.insert(&term.clone().into_term())?;
    }

    if full_book {
        for chapter in chapters.iter_mut() {
            let existing = chapter
                .meta
                .get("source_digest")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty());
            if force || existing.is_none() {
                let source = chapter
                    .segments
                    .iter()
                    .map(|segment| segment.source.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let user = serde_json::json!({
                    "chapter_id": chapter.id,
                    "title": chapter.title,
                    "source": sample_text(&source, CHAPTER_SAMPLE_CHARS),
                })
                .to_string();
                let response: DigestResponse = call_json(
                    client,
                    "TASK:CHAPTER_DIGEST Summarize this chapter in the source language for downstream translation context. Return only JSON with source_digest.",
                    &user,
                    max_retries,
                )
                .await?;
                let digest = if response.source_digest.trim().is_empty() {
                    // 部分兼容服务商会返回空的摘要字段，退回截取原文，保证初始化可继续。
                    sample_text(&source, 1_000)
                } else {
                    response.source_digest
                };
                chapter.meta["source_digest"] = serde_json::Value::String(digest);
                state::write_chapter(state_dir, project, chapter)?;
            }
        }

        if force || analysis.book_synopsis.as_deref().is_none_or(str::is_empty) {
            let digests = chapters
                .iter()
                .map(|chapter| {
                    serde_json::json!({
                        "chapter_id": chapter.id,
                        "title": chapter.title,
                        "source_digest": chapter.meta["source_digest"],
                    })
                })
                .collect::<Vec<_>>();
            let response: SynopsisResponse = call_json(
                client,
                "TASK:BOOK_SYNOPSIS Combine the ordered chapter digests into a concise whole-book synopsis. Return only JSON with book_synopsis.",
                &serde_json::json!({ "chapters": digests }).to_string(),
                max_retries,
            )
            .await?;
            analysis.book_synopsis = Some(if response.book_synopsis.trim().is_empty() {
                digests
                    .iter()
                    .filter_map(|chapter| chapter["source_digest"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                response.book_synopsis
            });
            state::write_json_atomic(&analysis_path, &analysis)?;
        }
    }

    Ok(analysis)
}

impl AnalysisTerm {
    /// 转为术语库条目，初始状态为正常、策略为自动。
    fn into_term(self) -> Term {
        Term {
            source: self.source,
            target: self.target,
            reading: self.reading,
            term_type: self.term_type,
            gender: self.gender,
            aliases: self.aliases,
            first_chapter: self.first_chapter,
            note: self.note,
            status: TermStatus::Ok,
            policy: crate::terms::TermPolicy::Automatic,
            manual_target: None,
        }
    }
}

/// 校验分析结果的必填字段与风格指南。
fn validate_analysis(analysis: &BookAnalysis) -> Result<(), String> {
    let fields = [
        ("genre", analysis.genre.as_str()),
        ("tone", analysis.tone.as_str()),
        ("narration", analysis.narration.as_str()),
        ("pacing", analysis.pacing.as_str()),
        ("register", analysis.register.as_str()),
        ("dialogue_style", analysis.dialogue_style.as_str()),
        ("rhetoric", analysis.rhetoric.as_str()),
    ];
    if let Some((name, _)) = fields.iter().find(|(_, value)| value.trim().is_empty()) {
        return Err(format!("book analysis field '{name}' is empty"));
    }
    if analysis.style_guide.is_empty()
        || analysis
            .style_guide
            .iter()
            .any(|item| item.trim().is_empty())
    {
        return Err("book analysis style_guide must contain non-empty items".to_string());
    }
    Ok(())
}

/// 请求结构化 JSON 并在失败时按 `max_retries` 指数退避重试。
async fn call_json<C, T>(
    client: &C,
    system: &str,
    user: &str,
    max_retries: usize,
) -> Result<T, String>
where
    C: TranslationClient + ?Sized,
    T: DeserializeOwned + JsonSchema,
{
    let schema = crate::schema::response_schema::<T>();
    let mut last_error = String::new();
    for attempt in 0..=max_retries {
        match client
            .complete_attempt(system, user, attempt, Some(schema.clone()))
            .await
        {
            Ok(output) => match crate::llm::parse_json_response(&output.text) {
                Ok(value) => return Ok(value),
                Err(error) => last_error = format!("LLM response is not valid JSON: {error}"),
            },
            Err(error) => last_error = error,
        }
        if attempt < max_retries {
            tokio::time::sleep(Duration::from_secs(1_u64 << attempt.min(6))).await;
        }
    }
    Err(format!(
        "analysis request failed after {max_retries} retries: {last_error}"
    ))
}

/// 用项目状态与章节还原文档模型，供语言检测使用。
fn document_from_state(project: &ProjectState, chapters: &[Chapter]) -> Document {
    Document {
        metadata: DocumentMetadata {
            title: project.title.clone(),
            source_language: project.source_language.clone(),
            target_language: project.target_language.clone(),
            source_format: Path::new(&project.source_file)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_string(),
        },
        chapters: chapters.to_vec(),
    }
}

/// 从全书均匀抽取三段文本作为分析样本。
fn sample_book(chapters: &[Chapter]) -> Vec<String> {
    let source = chapters
        .iter()
        .flat_map(|chapter| chapter.segments.iter())
        .map(|segment| segment.source.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    sample_positions(&source, BOOK_SAMPLE_CHARS, 3)
}

/// 取样文本：超长时取首中尾三段拼接，并用省略号分隔。
fn sample_text(source: &str, max_chars: usize) -> String {
    if source.chars().count() <= max_chars {
        source.to_string()
    } else {
        sample_positions(source, max_chars / 3, 3).join("\n…\n")
    }
}

/// 按固定间隔取 `count` 段等长文本，重复的短文本会被去重。
fn sample_positions(source: &str, sample_chars: usize, count: usize) -> Vec<String> {
    let chars = source.chars().collect::<Vec<_>>();
    if chars.is_empty() || sample_chars == 0 || count == 0 {
        return Vec::new();
    }
    let length = chars.len().min(sample_chars);
    let max_start = chars.len() - length;
    let mut samples = Vec::new();
    for index in 0..count {
        let start = if count == 1 {
            0
        } else {
            max_start * index / (count - 1)
        };
        let value = chars[start..start + length].iter().collect::<String>();
        if samples.last() != Some(&value) {
            samples.push(value);
        }
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::{prepare, sample_positions};
    use crate::llm::{CompletionOutput, MockClient, TranslationClient};
    use crate::model::{Chapter, Document, DocumentMetadata, ItemStatus, Segment, SegmentKind};
    use crate::state;
    use crate::terms::TermStore;
    use async_trait::async_trait;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct RejectingClient;

    #[async_trait]
    impl TranslationClient for RejectingClient {
        async fn complete(
            &self,
            _system_prompt: &str,
            _user_prompt: &str,
        ) -> Result<CompletionOutput, String> {
            Err("cached analysis unexpectedly called the model".to_string())
        }
    }

    fn temp_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "transitpls-analysis-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temp directory should be created");
        path
    }

    fn document() -> Document {
        Document {
            metadata: DocumentMetadata {
                title: "Book".to_string(),
                source_language: "en".to_string(),
                target_language: "zh-CN".to_string(),
                source_format: "txt".to_string(),
            },
            chapters: vec![Chapter {
                id: "chapter-1".to_string(),
                title: "Opening".to_string(),
                target_title: None,
                status: ItemStatus::Pending,
                meta: serde_json::json!({}),
                segments: vec![Segment {
                    id: "segment-1".to_string(),
                    ordinal: 0,
                    source: "Alice entered the city.".to_string(),
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
    fn samples_start_middle_and_end_without_duplicate_short_samples() {
        assert_eq!(sample_positions("短文", 10, 3), vec!["短文"]);
        assert_eq!(sample_positions("abcdefghij", 2, 3), vec!["ab", "ef", "ij"]);
    }

    #[test]
    fn analysis_prompt_limits_terms_to_proper_names_and_work_concepts() {
        let prompt = super::BOOK_ANALYSIS_PROMPT;
        assert!(prompt.contains("person names"));
        assert!(prompt.contains("Do not add ordinary nouns"));
        assert!(prompt.contains("chapter titles"));
        assert!(prompt.contains("Empty arrays are fine"));
    }

    #[tokio::test]
    async fn persists_mock_analysis_and_reuses_completed_work() {
        let dir = temp_dir();
        let source = dir.join("book.txt");
        let state_dir = dir.join("projects");
        fs::write(&source, "Alice entered the city.").expect("source should be written");
        let initialized =
            state::initialize(&state_dir, &source, &document(), 1_200).expect("init should work");
        let mut project = initialized.project;
        let mut chapters =
            state::load_chapters(&state_dir, &project).expect("chapters should load");

        let first = prepare(
            &MockClient,
            &state_dir,
            &mut project,
            &mut chapters,
            true,
            false,
            0,
        )
        .await
        .expect("mock analysis should complete");
        assert_eq!(
            first.book_synopsis.as_deref(),
            Some("Stable mock whole-book synopsis")
        );
        assert_eq!(
            chapters[0].meta["source_digest"].as_str(),
            Some("Mock digest for Opening")
        );
        let terms = TermStore::open(state::project_dir(&state_dir, &project.id).join("terms.db"))
            .expect("term store should open")
            .list()
            .expect("terms should list");
        assert_eq!(terms.len(), 2);

        prepare(
            &RejectingClient,
            &state_dir,
            &mut project,
            &mut chapters,
            true,
            false,
            0,
        )
        .await
        .expect("completed analysis should not call the model");
        fs::remove_dir_all(dir).expect("temp directory should be removed");
    }
}
