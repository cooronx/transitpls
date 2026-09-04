use crate::llm::{self, MockClient, RigClient, TranslationClient};
use crate::model::{Chapter, ItemStatus, ProjectStatus};
use crate::parser;
use crate::state;
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
    #[arg(long, global = true, default_value = "transitpls.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Init(InitArgs),
    Transit(TransitArgs),
    Review(InputArgs),
    Export(ExportArgs),
    Status(InputArgs),
}

#[derive(Debug, Args)]
struct InitArgs {
    input: PathBuf,
    #[arg(long)]
    source_language: Option<String>,
    #[arg(long, default_value_t = 2_000)]
    max_segment_chars: usize,
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
struct ExportArgs {
    #[arg(long)]
    format: String,
    input: PathBuf,
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
    match cli.command {
        Command::Init(args) => {
            if !args.input.is_file() {
                return Err(format!(
                    "input file does not exist: {}",
                    args.input.display()
                ));
            }
            let document = parser::parse_document(
                &args.input,
                args.source_language.as_deref(),
                args.max_segment_chars,
            )?;
            let initialized = state::initialize(&args.input, &document, args.max_segment_chars)?;
            println!(
                "{} project {} ({} chapters, source language {}, target language {})",
                if initialized.created {
                    "initialized"
                } else {
                    "already initialized"
                },
                initialized.project.id,
                initialized.project.chapters_total,
                initialized.project.source_language,
                initialized.project.target_language
            );
            Ok(0)
        }
        Command::Status(args) => {
            let project = state::load_for_source(&args.input)?;
            let chapters = state::load_chapters(&project)?;
            state::append_log(&project, "status_requested", serde_json::json!({}))?;
            print_status(&project, &chapters);
            Ok(0)
        }
        Command::Transit(args) => transit(args, cli.config).await,
        Command::Review(args) => {
            let project = state::load_for_source(&args.input)?;
            state::append_log(&project, "review_requested", serde_json::json!({}))?;
            eprintln!(
                "review is not implemented in this stage (project {})",
                project.id
            );
            Ok(2)
        }
        Command::Export(args) => {
            let project = state::load_for_source(&args.input)?;
            state::append_log(
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
    }
}

async fn transit(args: TransitArgs, config_path: PathBuf) -> Result<i32, String> {
    let mut project = state::load_for_source(&args.input)?;
    let mut chapters = state::load_chapters(&project)?;
    let client: Box<dyn TranslationClient> = if args.mock {
        Box::new(MockClient)
    } else {
        let config = llm::load_config(&config_path)?;
        Box::new(RigClient::from_config(&config.llm)?)
    };
    state::append_log(
        &project,
        "transit_started",
        serde_json::json!({ "mock": args.mock }),
    )?;
    project.status = ProjectStatus::Translating;
    state::save_progress(&mut project, &mut chapters)?;

    for chapter_index in 0..chapters.len() {
        let pending = chapters[chapter_index]
            .segments
            .iter()
            .filter(|segment| matches!(segment.status, ItemStatus::Pending | ItemStatus::Failed))
            .cloned()
            .collect::<Vec<_>>();
        for batch in pending.chunks(BATCH_SIZE) {
            let translations = match llm::translate_batch(
                client.as_ref(),
                batch,
                &project.source_language,
                &project.target_language,
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
                    state::save_progress(&mut project, &mut chapters)?;
                    state::mark_failed(&mut project, &error)?;
                    return Err(error);
                }
            };
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
            state::save_progress(&mut project, &mut chapters)?;
        }
    }
    state::append_log(
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
