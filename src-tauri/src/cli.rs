use crate::analysis;
use crate::config::{self, AppConfig};
use crate::llm::{self, MockClient, RigClient, TranslationClient};
use crate::model::{Chapter, ItemStatus, ProjectStatus};
use crate::parser;
use crate::state;
use crate::terms::{self, PendingExtraction, TermStore};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

const BATCH_SIZE: usize = 20;

#[derive(Debug, Parser)]
#[command(
    name = "transitpls-cli",
    version,
    about = "Long-form document translation workflow"
)]
pub struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Init(InitArgs),
    Transit(TransitArgs),
    Review(InputArgs),
    Export(ExportArgs),
    Status(ProjectArgs),
    Terms(TermsArgs),
}

#[derive(Debug, Args)]
struct InitArgs {
    input: PathBuf,
    #[arg(long)]
    source_language: Option<String>,
    #[arg(long)]
    max_segment_chars: Option<usize>,
    #[arg(long)]
    mock: bool,
    #[arg(long)]
    force_analysis: bool,
}

#[derive(Debug, Args)]
struct TransitArgs {
    input: PathBuf,
    #[arg(long)]
    mock: bool,
}

#[derive(Debug, Args)]
struct InputArgs {
    input: PathBuf,
}

#[derive(Debug, Args)]
struct ProjectArgs {
    #[arg(value_name = "INPUT", required_unless_present = "project")]
    input: Option<PathBuf>,
    #[arg(long, value_name = "SHA256", required_unless_present = "input")]
    project: Option<String>,
}

#[derive(Debug, Args)]
struct ExportArgs {
    #[arg(long)]
    format: String,
    input: PathBuf,
}

#[derive(Debug, Args)]
struct TermsArgs {
    #[command(subcommand)]
    command: TermsCommand,
}

#[derive(Debug, Subcommand)]
enum TermsCommand {
    List(ProjectArgs),
    Conflicts(ProjectArgs),
    Resolve(TermsResolveArgs),
}

#[derive(Debug, Args)]
struct TermsResolveArgs {
    #[arg(long, value_name = "SHA256")]
    project: Option<String>,
    #[arg(value_name = "INPUT SOURCE TARGET", num_args = 2..=3)]
    values: Vec<String>,
}

pub async fn run() -> i32 {
    let cli = Cli::parse();
    match execute(cli).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    }
}

async fn execute(cli: Cli) -> Result<i32, String> {
    let loaded = config::load(cli.config.as_deref())?;
    let state_dir = loaded.state_dir;
    let config = loaded.value;
    match cli.command {
        Command::Init(args) => {
            if !args.input.is_file() {
                return Err(format!(
                    "input file does not exist: {}",
                    args.input.display()
                ));
            }
            let existing = state::load_for_source(&state_dir, &args.input).ok();
            let (mut project, mut chapters, created) = if let Some(project) = existing {
                let chapters = state::load_chapters(&state_dir, &project)?;
                (project, chapters, false)
            } else {
                let requested_language = args
                    .source_language
                    .as_deref()
                    .unwrap_or(&config.language.source);
                let max_segment_chars = args
                    .max_segment_chars
                    .unwrap_or(config.segment.max_chars_per_segment);
                let mut document = parser::parse_document(
                    &args.input,
                    Some(requested_language),
                    max_segment_chars,
                )?;
                document.metadata.target_language = config.language.target.clone();
                let initialized =
                    state::initialize(&state_dir, &args.input, &document, max_segment_chars)?;
                let chapters = state::load_chapters(&state_dir, &initialized.project)?;
                (initialized.project, chapters, initialized.created)
            };
            if let Some(source_language) = args.source_language {
                project.source_language = source_language;
                state::save_project(&state_dir, &project)?;
            }
            let client = build_client(&config, args.mock)?;
            analysis::prepare(
                client.as_ref(),
                &state_dir,
                &mut project,
                &mut chapters,
                config.analysis.full_book,
                args.force_analysis,
                config.llm.max_retries,
            )
            .await?;
            state::append_log(
                &state_dir,
                &project,
                "analysis_completed",
                serde_json::json!({ "full_book": config.analysis.full_book }),
            )?;
            println!(
                "{} project {} ({} chapters, source language {}, target language {})",
                if created { "initialized" } else { "prepared" },
                project.id,
                project.chapters_total,
                project.source_language,
                project.target_language
            );
            Ok(0)
        }
        Command::Status(args) => {
            let project = load_project_args(&state_dir, &args)?;
            let chapters = state::load_chapters(&state_dir, &project)?;
            state::append_log(
                &state_dir,
                &project,
                "status_requested",
                serde_json::json!({}),
            )?;
            print_status(&project, &chapters);
            Ok(0)
        }
        Command::Transit(args) => transit(args, &state_dir, &config).await,
        Command::Review(args) => {
            let project = state::load_for_source(&state_dir, &args.input)?;
            state::append_log(
                &state_dir,
                &project,
                "review_requested",
                serde_json::json!({}),
            )?;
            eprintln!(
                "review is not implemented in this stage (project {})",
                project.id
            );
            Ok(2)
        }
        Command::Export(args) => {
            let project = state::load_for_source(&state_dir, &args.input)?;
            state::append_log(
                &state_dir,
                &project,
                "export_requested",
                serde_json::json!({ "format": args.format }),
            )?;
            eprintln!(
                "export is not implemented in this stage (project {})",
                project.id
            );
            Ok(2)
        }
        Command::Terms(args) => terms(args, &state_dir),
    }
}

fn terms(args: TermsArgs, state_dir: &std::path::Path) -> Result<i32, String> {
    match args.command {
        TermsCommand::List(selector) => {
            let project = load_project_args(state_dir, &selector)?;
            let store = term_store(state_dir, &project)?;
            println!("source\ttarget\ttype\tstatus\taliases");
            for term in store.list()? {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    term.source,
                    term.target,
                    term.term_type,
                    term.status,
                    term.aliases.join(", ")
                );
            }
            Ok(0)
        }
        TermsCommand::Conflicts(selector) => {
            let project = load_project_args(state_dir, &selector)?;
            let store = term_store(state_dir, &project)?;
            println!("source\tcandidate\tchapter");
            for conflict in store.conflicts()? {
                println!(
                    "{}\t{}\t{}",
                    conflict.source, conflict.target, conflict.chapter
                );
            }
            Ok(0)
        }
        TermsCommand::Resolve(args) => {
            let (project, source, target) = if let Some(id) = args.project {
                if args.values.len() != 2 {
                    return Err(
                        "terms resolve --project expects SOURCE and TARGET arguments".to_string(),
                    );
                }
                (
                    state::load_project(state_dir, &id)?,
                    &args.values[0],
                    &args.values[1],
                )
            } else {
                if args.values.len() != 3 {
                    return Err(
                        "terms resolve expects INPUT, SOURCE, and TARGET arguments".to_string()
                    );
                }
                (
                    state::load_for_source(state_dir, std::path::Path::new(&args.values[0]))?,
                    &args.values[1],
                    &args.values[2],
                )
            };
            let store = term_store(state_dir, &project)?;
            store.resolve(source, target)?;
            state::append_log(
                state_dir,
                &project,
                "term_resolved",
                serde_json::json!({ "source": source, "target": target }),
            )?;
            println!("resolved {source} -> {target}");
            Ok(0)
        }
    }
}

fn term_store(
    state_dir: &std::path::Path,
    project: &crate::model::ProjectState,
) -> Result<TermStore, String> {
    TermStore::open(state::project_dir(state_dir, &project.id).join("terms.db"))
}

async fn transit(
    args: TransitArgs,
    state_dir: &std::path::Path,
    config: &AppConfig,
) -> Result<i32, String> {
    let mut project = state::load_for_source(state_dir, &args.input)?;
    let mut chapters = state::load_chapters(state_dir, &project)?;
    let client = build_client(config, args.mock)?;
    let store = term_store(state_dir, &project)?;
    if let Err(error) = retry_pending_extractions(
        client.as_ref(),
        &store,
        state_dir,
        &project,
        &mut chapters,
        config.llm.max_retries,
    )
    .await
    {
        state::mark_failed(state_dir, &mut project, &error)?;
        return Err(error);
    }
    state::append_log(
        state_dir,
        &project,
        "transit_started",
        serde_json::json!({ "mock": args.mock }),
    )?;
    project.status = ProjectStatus::Translating;
    state::save_progress(state_dir, &mut project, &mut chapters)?;

    for chapter_index in 0..chapters.len() {
        let pending = chapters[chapter_index]
            .segments
            .iter()
            .filter(|segment| matches!(segment.status, ItemStatus::Pending | ItemStatus::Failed))
            .cloned()
            .collect::<Vec<_>>();
        for batch in pending.chunks(BATCH_SIZE) {
            let chapter_source = chapters[chapter_index]
                .segments
                .iter()
                .map(|segment| segment.source.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let relevant_terms = store.relevant(&chapter_source)?;
            let translations = match llm::translate_batch(
                client.as_ref(),
                batch,
                &project.source_language,
                &project.target_language,
                &relevant_terms,
                config.llm.max_retries,
            )
            .await
            {
                Ok(translations) => translations,
                Err(error) => {
                    for segment in &mut chapters[chapter_index].segments {
                        if batch.iter().any(|item| item.id == segment.id) {
                            segment.status = ItemStatus::Failed;
                        }
                    }
                    state::save_progress(state_dir, &mut project, &mut chapters)?;
                    state::mark_failed(state_dir, &mut project, &error)?;
                    return Err(error);
                }
            };
            let target_text = translations.join("\n");
            for (segment, translation) in batch.iter().zip(translations) {
                if let Some(stored) = chapters[chapter_index]
                    .segments
                    .iter_mut()
                    .find(|candidate| candidate.id == segment.id)
                {
                    stored.target = Some(translation);
                    stored.status = ItemStatus::Translated;
                }
            }
            if chapters[chapter_index]
                .segments
                .iter()
                .all(|segment| segment.status == ItemStatus::Translated)
            {
                chapters[chapter_index].status = ItemStatus::Translated;
            }
            let extraction = PendingExtraction {
                chapter_id: chapters[chapter_index].id.clone(),
                batch_key: batch
                    .iter()
                    .map(|segment| segment.id.as_str())
                    .collect::<Vec<_>>()
                    .join("|"),
                source_text: batch
                    .iter()
                    .map(|segment| segment.source.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                target_text,
            };
            record_pending_extraction(&mut chapters[chapter_index], &extraction)?;
            state::save_progress(state_dir, &mut project, &mut chapters)?;
            if let Err(error) = store.queue_extraction(&extraction) {
                state::mark_failed(state_dir, &mut project, &error)?;
                return Err(error);
            }
            if let Err(error) = process_extraction(
                client.as_ref(),
                &store,
                &extraction,
                chapter_index,
                config.llm.max_retries,
            )
            .await
            {
                state::mark_failed(state_dir, &mut project, &error)?;
                return Err(error);
            }
            clear_pending_extraction(&mut chapters[chapter_index], &extraction.batch_key);
            state::write_chapter(state_dir, &project, &chapters[chapter_index])?;
        }
        if chapters[chapter_index]
            .segments
            .iter()
            .all(|segment| segment.status == ItemStatus::Translated)
            && chapters[chapter_index].meta["terms_extracted"] != serde_json::Value::Bool(true)
        {
            let extraction = PendingExtraction {
                chapter_id: chapters[chapter_index].id.clone(),
                batch_key: "__chapter__".to_string(),
                source_text: chapters[chapter_index]
                    .segments
                    .iter()
                    .map(|segment| segment.source.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                target_text: chapters[chapter_index]
                    .segments
                    .iter()
                    .filter_map(|segment| segment.target.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            record_pending_extraction(&mut chapters[chapter_index], &extraction)?;
            state::write_chapter(state_dir, &project, &chapters[chapter_index])?;
            if let Err(error) = store.queue_extraction(&extraction) {
                state::mark_failed(state_dir, &mut project, &error)?;
                return Err(error);
            }
            if let Err(error) = process_extraction(
                client.as_ref(),
                &store,
                &extraction,
                chapter_index,
                config.llm.max_retries,
            )
            .await
            {
                state::mark_failed(state_dir, &mut project, &error)?;
                return Err(error);
            }
            clear_pending_extraction(&mut chapters[chapter_index], &extraction.batch_key);
            chapters[chapter_index].meta["terms_extracted"] = serde_json::Value::Bool(true);
            state::write_chapter(state_dir, &project, &chapters[chapter_index])?;
        }
    }
    state::append_log(
        state_dir,
        &project,
        "transit_completed",
        serde_json::json!({ "chapters": project.chapters_completed }),
    )?;
    println!(
        "translated {} chapters ({} segments)",
        project.chapters_completed,
        chapters
            .iter()
            .map(|chapter| chapter.segments.len())
            .sum::<usize>()
    );
    Ok(0)
}

async fn retry_pending_extractions<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    state_dir: &std::path::Path,
    project: &crate::model::ProjectState,
    chapters: &mut [Chapter],
    max_retries: usize,
) -> Result<(), String> {
    for chapter in chapters.iter() {
        if let Some(pending) = chapter
            .meta
            .get("pending_term_extractions")
            .and_then(serde_json::Value::as_object)
        {
            for value in pending.values() {
                let extraction: PendingExtraction = serde_json::from_value(value.clone())
                    .map_err(|error| format!("invalid pending term extraction state: {error}"))?;
                store.queue_extraction(&extraction)?;
            }
        }
    }
    for extraction in store.pending_extractions()? {
        let chapter_index = chapters
            .iter()
            .position(|chapter| chapter.id == extraction.chapter_id)
            .ok_or_else(|| {
                format!(
                    "pending term extraction references missing chapter {}",
                    extraction.chapter_id
                )
            })?;
        process_extraction(client, store, &extraction, chapter_index, max_retries).await?;
        clear_pending_extraction(&mut chapters[chapter_index], &extraction.batch_key);
        if extraction.batch_key == "__chapter__" {
            chapters[chapter_index].meta["terms_extracted"] = serde_json::Value::Bool(true);
        }
        state::write_chapter(state_dir, project, &chapters[chapter_index])?;
    }
    Ok(())
}

fn record_pending_extraction(
    chapter: &mut Chapter,
    extraction: &PendingExtraction,
) -> Result<(), String> {
    if !chapter.meta.is_object() {
        chapter.meta = serde_json::json!({});
    }
    let pending = chapter
        .meta
        .as_object_mut()
        .expect("chapter meta was normalized to an object")
        .entry("pending_term_extractions")
        .or_insert_with(|| serde_json::json!({}));
    let object = pending
        .as_object_mut()
        .ok_or_else(|| "chapter pending_term_extractions must be an object".to_string())?;
    object.insert(
        extraction.batch_key.clone(),
        serde_json::to_value(extraction)
            .map_err(|error| format!("failed to store pending extraction: {error}"))?,
    );
    Ok(())
}

fn clear_pending_extraction(chapter: &mut Chapter, batch_key: &str) {
    let Some(meta) = chapter.meta.as_object_mut() else {
        return;
    };
    let should_remove = meta
        .get_mut("pending_term_extractions")
        .and_then(serde_json::Value::as_object_mut)
        .is_some_and(|pending| {
            pending.remove(batch_key);
            pending.is_empty()
        });
    if should_remove {
        meta.remove("pending_term_extractions");
    }
}

async fn process_extraction<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    extraction: &PendingExtraction,
    chapter_index: usize,
    max_retries: usize,
) -> Result<(), String> {
    let extracted = terms::extract_terms(
        client,
        &extraction.source_text,
        &extraction.target_text,
        chapter_index,
        max_retries,
    )
    .await?;
    for term in extracted {
        store.insert(&term)?;
    }
    store.complete_extraction(&extraction.chapter_id, &extraction.batch_key)
}

fn build_client(config: &AppConfig, mock: bool) -> Result<Box<dyn TranslationClient>, String> {
    if mock {
        Ok(Box::new(MockClient))
    } else {
        Ok(Box::new(RigClient::from_config(&config.llm)?))
    }
}

fn load_project_args(
    state_dir: &std::path::Path,
    args: &ProjectArgs,
) -> Result<crate::model::ProjectState, String> {
    match (&args.input, &args.project) {
        (Some(input), None) => state::load_for_source(state_dir, input),
        (None, Some(id)) => state::load_project(state_dir, id),
        _ => Err("provide either an input file or --project <sha256>".to_string()),
    }
}

fn print_status(project: &crate::model::ProjectState, chapters: &[Chapter]) {
    println!("project: {}", project.id);
    println!("title: {}", project.title);
    println!("status: {:?}", project.status);
    println!("source: {}", project.source_file);
    println!(
        "languages: {} -> {}",
        project.source_language, project.target_language
    );
    println!(
        "chapters: {}/{} completed",
        project.chapters_completed, project.chapters_total
    );
    for (index, chapter) in chapters.iter().enumerate() {
        let translated = chapter
            .segments
            .iter()
            .filter(|segment| segment.status == ItemStatus::Translated)
            .count();
        println!(
            "  {}. {} [{:?}] {}/{} segments",
            index + 1,
            chapter.title,
            chapter.status,
            translated,
            chapter.segments.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{transit, Cli, TransitArgs};
    use crate::config::AppConfig;
    use crate::terms::TermStore;
    use crate::{parser, state};
    use clap::Parser;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("transitpls-cli-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).expect("temp directory should be created");
        path
    }

    #[test]
    fn accepts_explicit_config_and_project_id() {
        let cli = Cli::try_parse_from([
            "transitpls-cli",
            "--config",
            "custom.toml",
            "status",
            "--project",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ])
        .expect("CLI arguments should parse");
        assert_eq!(cli.config, Some(PathBuf::from("custom.toml")));
    }

    #[tokio::test]
    async fn mock_transit_extracts_terms_after_saving_translation() {
        let dir = temp_dir();
        let source = dir.join("book.txt");
        let state_dir = dir.join("projects");
        fs::write(&source, "Chapter 1\n\nAlice entered the city.")
            .expect("source should be written");
        let document =
            parser::parse_document(&source, Some("en"), 1_200).expect("document should parse");
        let initialized = state::initialize(&state_dir, &source, &document, 1_200)
            .expect("project should initialize");

        transit(
            TransitArgs {
                input: source.clone(),
                mock: true,
            },
            &state_dir,
            &AppConfig::default(),
        )
        .await
        .expect("mock transit should complete");

        let chapters =
            state::load_chapters(&state_dir, &initialized.project).expect("chapters should load");
        assert!(chapters[0]
            .segments
            .iter()
            .all(|segment| segment.target.is_some()));
        assert_eq!(
            chapters[0].meta["terms_extracted"],
            serde_json::Value::Bool(true)
        );
        assert!(chapters[0].meta.get("pending_term_extractions").is_none());
        let store = TermStore::open(
            state::project_dir(&state_dir, &initialized.project.id).join("terms.db"),
        )
        .expect("term store should open");
        assert!(store
            .list()
            .expect("terms should list")
            .iter()
            .any(|term| term.source == "Alice"));
        assert!(store
            .pending_extractions()
            .expect("pending extractions should list")
            .is_empty());
        fs::remove_dir_all(dir).expect("temp directory should be removed");
    }
}
