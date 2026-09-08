use crate::llm::TranslationClient;
use crate::model::{Chapter, Document, DocumentMetadata, ProjectState};
use crate::state;
use crate::terms::{Term, TermStatus, TermStore};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

const BOOK_SAMPLE_CHARS: usize = 4_000;
const CHAPTER_SAMPLE_CHARS: usize = 12_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BookAnalysis {
    pub genre: String,
    pub tone: String,
    pub style_guide: Vec<String>,
    pub narration: String,
    pub pacing: String,
    pub register: String,
    pub dialogue_style: String,
    pub rhetoric: String,
    pub characters: Vec<AnalysisTerm>,
    pub terms: Vec<AnalysisTerm>,
    pub book_synopsis: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnalysisTerm {
    pub source: String,
    pub target: String,
    pub reading: Option<String>,
    #[serde(rename = "type")]
    pub term_type: String,
    pub gender: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub first_chapter: usize,
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DigestResponse {
    source_digest: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SynopsisResponse {
    book_synopsis: String,
}

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
        let mut value: BookAnalysis = call_json(
            client,
            "TASK:BOOK_STYLE_ANALYSIS Analyze the whole book's style. Return only JSON with string fields genre, tone, narration, pacing, register, dialogue_style, and rhetoric; style_guide must be an array of non-empty strings; characters and terms must be arrays; book_synopsis must be null. Every character and term object must contain string source and target, nullable string reading and gender, string-array aliases, zero-based integer first_chapter, nullable string note, and type chosen from person, place, organization, term, appellation, speech, or fixed_expr.",
            &user,
            max_retries,
        )
        .await?;
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
                    // Some compatible providers return an empty structured field; retain
                    // useful chapter context so initialization can still resume.
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
        }
    }
}

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

async fn call_json<C, T>(
    client: &C,
    system: &str,
    user: &str,
    max_retries: usize,
) -> Result<T, String>
where
    C: TranslationClient + ?Sized,
    T: DeserializeOwned,
{
    let mut last_error = String::new();
    for attempt in 0..=max_retries {
        match client.complete_attempt(system, user, attempt).await {
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

fn sample_book(chapters: &[Chapter]) -> Vec<String> {
    let source = chapters
        .iter()
        .flat_map(|chapter| chapter.segments.iter())
        .map(|segment| segment.source.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    sample_positions(&source, BOOK_SAMPLE_CHARS, 3)
}

fn sample_text(source: &str, max_chars: usize) -> String {
    if source.chars().count() <= max_chars {
        source.to_string()
    } else {
        sample_positions(source, max_chars / 3, 3).join("\n…\n")
    }
}

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
