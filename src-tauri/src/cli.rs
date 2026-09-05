use crate::config::{self, AppConfig};
use crate::llm::{self, MockClient, RigClient, TranslationClient};
use crate::model::{Chapter, ItemStatus, ProjectStatus};
use crate::parser;
use crate::state;
use crate::terms::TermStore;
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
            if let Ok(project) = state::load_for_source(&state_dir, &args.input) {
                state::append_log(&state_dir, &project, "init_exists", serde_json::json!({}))?;
                println!(
                    "already initialized project {} ({} chapters, source language {}, target language {})",
                    project.id,
                    project.chapters_total,
                    project.source_language,
                    project.target_language
                );
                return Ok(0);
            }
            let requested_language = args
                .source_language
                .as_deref()
                .unwrap_or(&config.language.source);
            let max_segment_chars = args
                .max_segment_chars
                .unwrap_or(config.segment.max_chars_per_segment);
            let mut document =
                parser::parse_document(&args.input, Some(requested_language), max_segment_chars)?;
            if requested_language == "auto" {
                let client = build_client(&config, args.mock)?;
                document.metadata.source_language =
                    llm::detect_source_language(client.as_ref(), &document).await?;
            }
            document.metadata.target_language = config.language.target.clone();
            let initialized =
                state::initialize(&state_dir, &args.input, &document, max_segment_chars)?;
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
                    "{}\t{}\t{}\t{:?}\t{}",
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
            let translations = match llm::translate_batch(
                client.as_ref(),
                batch,
                &project.source_language,
                &project.target_language,
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
            state::save_progress(state_dir, &mut project, &mut chapters)?;
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
    use super::Cli;
    use clap::Parser;
    use std::path::PathBuf;

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
}
