use crate::analysis;
use crate::config::{self, AppConfig};
use crate::export::{self, ExportFormat};
use crate::llm::{self, MockClient, RecordingClient, RigClient, TranslationClient};
use crate::model::{Chapter, ItemStatus, ProjectStatus, Segment, SegmentKind};
use crate::parser;
use crate::pipeline;
use crate::review;
use crate::state;
use crate::terms::{self, PendingExtraction, TermStore};
use crate::usage::UsageRecorder;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

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
    Review(ReviewArgs),
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
    chapter: Option<usize>,
    #[arg(long)]
    mock: bool,
}

#[derive(Debug, Args)]
struct InputArgs {
    input: PathBuf,
}
#[derive(Debug, Args)]
struct ReviewArgs {
    input: Option<PathBuf>,
    #[arg(long)]
    project: Option<String>,
    #[arg(long)]
    chapter: Option<usize>,
    #[arg(long)]
    severity: Option<String>,
    #[arg(long, default_value = "text")]
    format: String,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long)]
    mock: bool,
    #[arg(long)]
    resume: Option<String>,
    #[arg(long)]
    retry_failed: bool,
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
    format: ExportFormatArg,
    #[arg(long)]
    out: Option<PathBuf>,
    input: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ExportFormatArg {
    Txt,
    Epub,
}

impl From<ExportFormatArg> for ExportFormat {
    fn from(value: ExportFormatArg) -> Self {
        match value {
            ExportFormatArg::Txt => Self::Txt,
            ExportFormatArg::Epub => Self::Epub,
        }
    }
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
    let config_path = cli.config.clone();
    let loaded = config::load(config_path.as_deref())?;
    let state_dir = loaded.state_dir;
    let config = loaded.value;
    match cli.command {
        Command::Init(args) => {
            let (project, created) = initialize_project(
                config_path,
                args.input,
                args.source_language,
                args.max_segment_chars,
                args.mock,
                args.force_analysis,
            )
            .await?;
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
        Command::Review(args) => review_command(args, &state_dir).await,
        Command::Export(args) => export_file(args, &state_dir),
        Command::Terms(args) => terms(args, &state_dir),
    }
}

async fn review_command(args: ReviewArgs, state_dir: &std::path::Path) -> Result<i32, String> {
    let project = if let Some(id) = args.project {
        state::load_project(state_dir, &id)?
    } else if let Some(input) = args.input {
        state::load_for_source(state_dir, &input)?
    } else {
        return Err("review requires INPUT or --project".into());
    };
    let chapters = state::load_chapters(state_dir, &project)?;
    if let Some(index) = args.chapter {
        if index >= chapters.len() {
            return Err(format!("chapter index {index} is out of range"));
        }
    }
    let (report, dir) = review::run(
        state_dir,
        &project,
        &chapters,
        args.chapter,
        args.mock,
        args.resume.as_deref(),
        args.retry_failed,
    )
    .await?;
    if let Some(out) = args.out {
        std::fs::copy(
            if args.format == "json" {
                dir.join("report.json")
            } else {
                dir.join("report.md")
            },
            &out,
        )
        .map_err(|e| format!("copy report: {e}"))?;
    }
    if args.format == "json" {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "review completed: project {}\nrun: {}\nissues: {}\nreport: {}",
            project.id,
            dir.display(),
            report.issues.len(),
            dir.join("report.md").display()
        );
    }
    Ok(if report.failed_batches.is_empty() {
        0
    } else {
        2
    })
}

pub async fn initialize_project(
    config_path: Option<PathBuf>,
    input: PathBuf,
    source_language: Option<String>,
    max_segment_chars: Option<usize>,
    mock_client: bool,
    force_analysis: bool,
) -> Result<(crate::model::ProjectState, bool), String> {
    if !input.is_file() {
        return Err(format!("input file does not exist: {}", input.display()));
    }
    let loaded = config::load(config_path.as_deref())?;
    let state_dir = loaded.state_dir;
    let config = loaded.value;
    let existing = state::load_for_source(&state_dir, &input).ok();
    let (mut project, mut chapters, created) = if let Some(project) = existing {
        let chapters = state::load_chapters(&state_dir, &project)?;
        (project, chapters, false)
    } else {
        let requested_language = source_language
            .as_deref()
            .unwrap_or(&config.language.source);
        let max_segment_chars = max_segment_chars.unwrap_or(config.segment.max_chars_per_segment);
        let mut document =
            parser::parse_document(&input, Some(requested_language), max_segment_chars)?;
        document.metadata.target_language = config.language.target.clone();
        let initialized = state::initialize(&state_dir, &input, &document, max_segment_chars)?;
        let chapters = state::load_chapters(&state_dir, &initialized.project)?;
        (initialized.project, chapters, initialized.created)
    };
    let _lock = state::acquire_project_lock(&state_dir, &project)?;
    if let Some(source_language) = source_language {
        project.source_language = source_language;
        state::save_project(&state_dir, &project)?;
    }
    let result = async {
        let client = build_client(&config, mock_client, &state_dir, &project)?;
        analysis::prepare(
            client.as_ref(),
            &state_dir,
            &mut project,
            &mut chapters,
            config.analysis.full_book,
            force_analysis,
            config.llm.max_retries,
        )
        .await
    }
    .await;
    if let Err(error) = result {
        state::mark_failed(&state_dir, &mut project, &error).map_err(|log_error| {
            format!("{error}; failed to record initialization error: {log_error}")
        })?;
        return Err(error);
    }
    state::append_log(
        &state_dir,
        &project,
        "analysis_completed",
        serde_json::json!({ "full_book": config.analysis.full_book }),
    )?;
    Ok((project, created))
}

pub fn import_project(
    config_path: Option<PathBuf>,
    input: PathBuf,
) -> Result<(crate::model::ProjectState, bool), String> {
    if !input.is_file() {
        return Err(format!("input file does not exist: {}", input.display()));
    }
    let loaded = config::load(config_path.as_deref())?;
    if let Ok(project) = state::load_for_source(&loaded.state_dir, &input) {
        return Ok((project, false));
    }

    let config = loaded.value;
    let mut document = parser::parse_document(
        &input,
        Some(&config.language.source),
        config.segment.max_chars_per_segment,
    )?;
    document.metadata.target_language = config.language.target;
    let initialized = state::initialize(
        &loaded.state_dir,
        &input,
        &document,
        config.segment.max_chars_per_segment,
    )?;
    Ok((initialized.project, initialized.created))
}

pub async fn transit_project(
    config_path: Option<PathBuf>,
    input: PathBuf,
    chapter: Option<usize>,
    mock_client: bool,
) -> Result<crate::model::ProjectState, String> {
    let loaded = config::load(config_path.as_deref())?;
    transit(
        TransitArgs {
            input: input.clone(),
            chapter,
            mock: mock_client,
        },
        &loaded.state_dir,
        &loaded.value,
    )
    .await?;
    state::load_for_source(&loaded.state_dir, &input)
}

fn export_file(args: ExportArgs, state_dir: &std::path::Path) -> Result<i32, String> {
    let format = ExportFormat::from(args.format);
    let output = args
        .out
        .unwrap_or_else(|| export::default_output_path(&args.input, format));
    let snapshot = state::load_export_snapshot(state_dir, &args.input)?;
    let bytes = match format {
        ExportFormat::Txt => export::render_txt(&snapshot)?.into_bytes(),
        ExportFormat::Epub => export::render_epub(&snapshot)?,
    };
    export::write_atomic(&output, &bytes)?;
    println!("exported {}", output.display());
    Ok(0)
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
            for conflict in store.alias_conflicts()? {
                println!(
                    "{}\talias shared by {} and {}\t-",
                    conflict.alias, conflict.first_source, conflict.second_source
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
            let _lock = state::acquire_project_lock(state_dir, &project)?;
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
    let _lock = state::acquire_project_lock(state_dir, &project)?;
    let mut chapters = state::load_chapters(state_dir, &project)?;
    if let Some(chapter) = args.chapter {
        if chapter >= chapters.len() {
            return Err(format!(
                "chapter index {chapter} is out of range; project has {} chapters",
                chapters.len()
            ));
        }
    }
    let analysis = load_translation_analysis(state_dir, &project, &chapters, config)?;
    let client = build_client(config, args.mock, state_dir, &project)?;
    let store = term_store(state_dir, &project)?;
    state::append_log(
        state_dir,
        &project,
        "transit_started",
        serde_json::json!({ "mock": args.mock, "chapter": args.chapter }),
    )?;
    project.status = ProjectStatus::Translating;
    state::save_project(state_dir, &project)?;

    let result = run_transit(
        client.as_ref(),
        &store,
        state_dir,
        &mut project,
        &mut chapters,
        &analysis,
        config,
        args.chapter,
    )
    .await;
    if let Err(error) = result {
        state::mark_failed(state_dir, &mut project, &error)?;
        return Err(error);
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

#[allow(clippy::too_many_arguments)]
async fn run_transit<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    state_dir: &std::path::Path,
    project: &mut crate::model::ProjectState,
    chapters: &mut [Chapter],
    analysis: &analysis::BookAnalysis,
    config: &AppConfig,
    selected_chapter: Option<usize>,
) -> Result<(), String> {
    retry_pending_extractions(
        client,
        store,
        state_dir,
        project,
        chapters,
        config.llm.max_retries,
        config.pipeline.recent_context_chars,
    )
    .await?;

    let chapter_indices = selected_chapter
        .map(|index| vec![index])
        .unwrap_or_else(|| (0..chapters.len()).collect());
    for chapter_index in chapter_indices {
        translate_chapter(
            client,
            store,
            state_dir,
            project,
            chapters,
            chapter_index,
            analysis,
            config,
        )
        .await?;
    }

    if chapters.iter().all(chapter_body_complete) {
        translate_missing_titles(client, state_dir, project, chapters, analysis, config).await?;
    }
    save_project_progress(state_dir, project, chapters)
}

#[allow(clippy::too_many_arguments)]
async fn translate_chapter<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    state_dir: &std::path::Path,
    project: &mut crate::model::ProjectState,
    chapters: &mut [Chapter],
    chapter_index: usize,
    analysis: &analysis::BookAnalysis,
    config: &AppConfig,
) -> Result<(), String> {
    let chapter_source = chapters[chapter_index]
        .segments
        .iter()
        .map(|segment| segment.source.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let relevant_terms = store.relevant(&chapter_source)?;
    let digest = chapters[chapter_index]
        .meta
        .get("source_digest")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let ranges = pipeline::batch_ranges(
        &chapters[chapter_index].segments,
        config.segment.max_chars_per_batch,
    );

    for range in ranges {
        let promoted = range
            .clone()
            .filter(|&index| {
                !config.pipeline.polish
                    && chapters[chapter_index].segments[index].target.is_none()
                    && chapters[chapter_index].segments[index]
                        .target_before_polish
                        .is_some()
            })
            .collect::<Vec<_>>();
        for &index in &promoted {
            let segment = &mut chapters[chapter_index].segments[index];
            segment.target = segment.target_before_polish.clone();
            segment.status = ItemStatus::Translated;
        }
        finalize_segments(
            client,
            store,
            state_dir,
            project,
            chapters,
            chapter_index,
            &promoted,
            config,
        )
        .await?;

        let untranslated = range
            .clone()
            .filter(|&index| {
                let segment = &chapters[chapter_index].segments[index];
                segment.target.is_none() && segment.target_before_polish.is_none()
            })
            .collect::<Vec<_>>();
        if !untranslated.is_empty() {
            let request = cloned_segments(chapters, chapter_index, &untranslated);
            let recent = pipeline::recent_targets(
                chapters,
                chapter_index,
                untranslated[0],
                config.pipeline.recent_context_chars,
            );
            match request_translation(
                client,
                &request,
                project,
                analysis,
                digest.as_deref(),
                &relevant_terms,
                &recent,
                config.llm.max_retries,
            )
            .await
            {
                Ok(translations) => {
                    for (&index, translation) in untranslated.iter().zip(translations) {
                        store_draft_or_final(
                            &mut chapters[chapter_index].segments[index],
                            translation,
                            config.pipeline.polish,
                        );
                    }
                    state::write_chapter(state_dir, project, &chapters[chapter_index])?;
                    if !config.pipeline.polish {
                        finalize_segments(
                            client,
                            store,
                            state_dir,
                            project,
                            chapters,
                            chapter_index,
                            &untranslated,
                            config,
                        )
                        .await?;
                    }
                }
                Err(_) => {
                    for &index in &untranslated {
                        translate_one_with_fallback(
                            client,
                            store,
                            state_dir,
                            project,
                            chapters,
                            chapter_index,
                            index,
                            analysis,
                            digest.as_deref(),
                            &relevant_terms,
                            config,
                        )
                        .await?;
                    }
                }
            }
        }

        if config.pipeline.polish {
            let unpolished = range
                .clone()
                .filter(|&index| {
                    let segment = &chapters[chapter_index].segments[index];
                    segment.target.is_none() && segment.target_before_polish.is_some()
                })
                .collect::<Vec<_>>();
            polish_segments(
                client,
                store,
                state_dir,
                project,
                chapters,
                chapter_index,
                &unpolished,
                analysis,
                digest.as_deref(),
                &relevant_terms,
                config,
            )
            .await?;
        }

        pipeline::write_context(
            &state::project_dir(state_dir, &project.id),
            chapters,
            chapter_index,
            range.end,
            config.pipeline.recent_context_chars,
        )?;
    }

    extract_completed_chapter(
        client,
        store,
        state_dir,
        project,
        chapters,
        chapter_index,
        config,
    )
    .await
}

fn cloned_segments(chapters: &[Chapter], chapter_index: usize, indices: &[usize]) -> Vec<Segment> {
    indices
        .iter()
        .map(|&index| chapters[chapter_index].segments[index].clone())
        .collect()
}

fn store_draft_or_final(segment: &mut Segment, value: String, polish: bool) {
    if polish {
        segment.target_before_polish = Some(value);
        segment.target = None;
        segment.status = ItemStatus::Pending;
    } else {
        segment.target = Some(value);
        segment.status = ItemStatus::Translated;
    }
}

#[allow(clippy::too_many_arguments)]
async fn request_translation<C: TranslationClient + ?Sized>(
    client: &C,
    segments: &[Segment],
    project: &crate::model::ProjectState,
    analysis: &analysis::BookAnalysis,
    digest: Option<&str>,
    terms: &[crate::terms::Term],
    recent: &[llm::RecentTarget],
    max_retries: usize,
) -> Result<Vec<String>, String> {
    llm::translate_batch(
        client,
        segments,
        &project.source_language,
        &project.target_language,
        &llm::TranslationContext {
            style_guide: &analysis.style_guide,
            book_synopsis: analysis.book_synopsis.as_deref(),
            chapter_digest: digest,
            terms,
            recent_targets: recent,
        },
        max_retries,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn translate_one_with_fallback<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    state_dir: &std::path::Path,
    project: &mut crate::model::ProjectState,
    chapters: &mut [Chapter],
    chapter_index: usize,
    segment_index: usize,
    analysis: &analysis::BookAnalysis,
    digest: Option<&str>,
    terms: &[crate::terms::Term],
    config: &AppConfig,
) -> Result<(), String> {
    let recent = pipeline::recent_targets(
        chapters,
        chapter_index,
        segment_index,
        config.pipeline.recent_context_chars,
    );
    let segment = chapters[chapter_index].segments[segment_index].clone();
    let translations = request_translation(
        client,
        std::slice::from_ref(&segment),
        project,
        analysis,
        digest,
        terms,
        &recent,
        config.llm.max_retries,
    )
    .await;
    let translation = match translations {
        Ok(mut values) => values.remove(0),
        Err(error) => {
            chapters[chapter_index].segments[segment_index].status = ItemStatus::Failed;
            state::write_chapter(state_dir, project, &chapters[chapter_index])?;
            return Err(format!("segment {} failed: {error}", segment.id));
        }
    };
    store_draft_or_final(
        &mut chapters[chapter_index].segments[segment_index],
        translation,
        config.pipeline.polish,
    );
    state::write_chapter(state_dir, project, &chapters[chapter_index])?;

    if config.pipeline.polish {
        polish_segments(
            client,
            store,
            state_dir,
            project,
            chapters,
            chapter_index,
            &[segment_index],
            analysis,
            digest,
            terms,
            config,
        )
        .await
    } else {
        finalize_segments(
            client,
            store,
            state_dir,
            project,
            chapters,
            chapter_index,
            &[segment_index],
            config,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn polish_segments<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    state_dir: &std::path::Path,
    project: &mut crate::model::ProjectState,
    chapters: &mut [Chapter],
    chapter_index: usize,
    indices: &[usize],
    analysis: &analysis::BookAnalysis,
    digest: Option<&str>,
    terms: &[crate::terms::Term],
    config: &AppConfig,
) -> Result<(), String> {
    if indices.is_empty() {
        return Ok(());
    }
    let request = cloned_segments(chapters, chapter_index, indices);
    let recent = pipeline::recent_targets(
        chapters,
        chapter_index,
        indices[0],
        config.pipeline.recent_context_chars,
    );
    let context = llm::TranslationContext {
        style_guide: &analysis.style_guide,
        book_synopsis: analysis.book_synopsis.as_deref(),
        chapter_digest: digest,
        terms,
        recent_targets: &recent,
    };
    match llm::polish_batch(client, &request, &context, config.llm.max_retries).await {
        Ok(translations) => {
            for (&index, translation) in indices.iter().zip(translations) {
                let segment = &mut chapters[chapter_index].segments[index];
                segment.target = Some(translation);
                segment.status = ItemStatus::Translated;
            }
            finalize_segments(
                client,
                store,
                state_dir,
                project,
                chapters,
                chapter_index,
                indices,
                config,
            )
            .await
        }
        Err(_) => {
            for &index in indices {
                let request = chapters[chapter_index].segments[index].clone();
                let recent = pipeline::recent_targets(
                    chapters,
                    chapter_index,
                    index,
                    config.pipeline.recent_context_chars,
                );
                let context = llm::TranslationContext {
                    style_guide: &analysis.style_guide,
                    book_synopsis: analysis.book_synopsis.as_deref(),
                    chapter_digest: digest,
                    terms,
                    recent_targets: &recent,
                };
                let polished = llm::polish_batch(
                    client,
                    std::slice::from_ref(&request),
                    &context,
                    config.llm.max_retries,
                )
                .await;
                let polished = match polished {
                    Ok(mut values) => values.remove(0),
                    Err(error) => {
                        chapters[chapter_index].segments[index].status = ItemStatus::Failed;
                        state::write_chapter(state_dir, project, &chapters[chapter_index])?;
                        return Err(format!("segment {} polish failed: {error}", request.id));
                    }
                };
                chapters[chapter_index].segments[index].target = Some(polished);
                chapters[chapter_index].segments[index].status = ItemStatus::Translated;
                finalize_segments(
                    client,
                    store,
                    state_dir,
                    project,
                    chapters,
                    chapter_index,
                    &[index],
                    config,
                )
                .await?;
            }
            Ok(())
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn finalize_segments<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    state_dir: &std::path::Path,
    project: &mut crate::model::ProjectState,
    chapters: &mut [Chapter],
    chapter_index: usize,
    indices: &[usize],
    config: &AppConfig,
) -> Result<(), String> {
    if indices.is_empty() {
        return Ok(());
    }
    for &index in indices {
        chapters[chapter_index].segments[index].status = ItemStatus::Translated;
    }
    chapters[chapter_index].status = if chapter_body_complete(&chapters[chapter_index]) {
        ItemStatus::Translated
    } else {
        ItemStatus::Pending
    };
    let extraction = PendingExtraction {
        chapter_id: chapters[chapter_index].id.clone(),
        batch_key: indices
            .iter()
            .map(|&index| chapters[chapter_index].segments[index].id.as_str())
            .collect::<Vec<_>>()
            .join("|"),
        source_text: indices
            .iter()
            .map(|&index| chapters[chapter_index].segments[index].source.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        target_text: indices
            .iter()
            .filter_map(|&index| chapters[chapter_index].segments[index].target.as_deref())
            .collect::<Vec<_>>()
            .join("\n"),
    };
    record_pending_extraction(&mut chapters[chapter_index], &extraction)?;
    state::write_chapter(state_dir, project, &chapters[chapter_index])?;
    pipeline::write_context(
        &state::project_dir(state_dir, &project.id),
        chapters,
        chapter_index,
        indices.iter().copied().max().unwrap_or_default() + 1,
        config.pipeline.recent_context_chars,
    )?;
    save_project_progress(state_dir, project, chapters)?;
    store.queue_extraction(&extraction)?;
    process_extraction(
        client,
        store,
        &extraction,
        chapter_index,
        config.llm.max_retries,
    )
    .await?;
    clear_pending_extraction(&mut chapters[chapter_index], &extraction.batch_key);
    state::write_chapter(state_dir, project, &chapters[chapter_index])
}

async fn extract_completed_chapter<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    state_dir: &std::path::Path,
    project: &mut crate::model::ProjectState,
    chapters: &mut [Chapter],
    chapter_index: usize,
    config: &AppConfig,
) -> Result<(), String> {
    if !chapter_body_complete(&chapters[chapter_index])
        || chapters[chapter_index].meta["terms_extracted"] == serde_json::Value::Bool(true)
    {
        return Ok(());
    }
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
    state::write_chapter(state_dir, project, &chapters[chapter_index])?;
    pipeline::write_context(
        &state::project_dir(state_dir, &project.id),
        chapters,
        chapter_index,
        chapters[chapter_index].segments.len(),
        config.pipeline.recent_context_chars,
    )?;
    store.queue_extraction(&extraction)?;
    process_extraction(
        client,
        store,
        &extraction,
        chapter_index,
        config.llm.max_retries,
    )
    .await?;
    clear_pending_extraction(&mut chapters[chapter_index], &extraction.batch_key);
    chapters[chapter_index].meta["terms_extracted"] = serde_json::Value::Bool(true);
    state::write_chapter(state_dir, project, &chapters[chapter_index])
}

async fn translate_missing_titles<C: TranslationClient + ?Sized>(
    client: &C,
    state_dir: &std::path::Path,
    project: &mut crate::model::ProjectState,
    chapters: &mut [Chapter],
    analysis: &analysis::BookAnalysis,
    config: &AppConfig,
) -> Result<(), String> {
    for chapter in chapters.iter_mut() {
        if let Some(target_title) = chapter.target_title.clone() {
            sync_heading_title(chapter, &target_title);
        }
    }
    let title_segments = chapters
        .iter()
        .map(|chapter| Segment {
            id: chapter.id.clone(),
            ordinal: 0,
            source: chapter.title.clone(),
            target: chapter.target_title.clone(),
            target_before_polish: None,
            kind: SegmentKind::Heading,
            status: if chapter.target_title.is_some() {
                ItemStatus::Translated
            } else {
                ItemStatus::Pending
            },
            source_hash: String::new(),
            meta: serde_json::json!({}),
        })
        .collect::<Vec<_>>();
    for range in pipeline::batch_ranges(&title_segments, config.segment.max_chars_per_batch) {
        let indices = range
            .filter(|&index| chapters[index].target_title.is_none())
            .collect::<Vec<_>>();
        if indices.is_empty() {
            continue;
        }
        let request = indices
            .iter()
            .map(|&index| title_segments[index].clone())
            .collect::<Vec<_>>();
        match llm::translate_titles(
            client,
            &request,
            &project.source_language,
            &project.target_language,
            &analysis.style_guide,
            config.llm.max_retries,
        )
        .await
        {
            Ok(translations) => {
                for (&index, translation) in indices.iter().zip(translations) {
                    apply_title(state_dir, project, chapters, index, translation)?;
                }
            }
            Err(_) => {
                for &index in &indices {
                    let translation = llm::translate_titles(
                        client,
                        std::slice::from_ref(&title_segments[index]),
                        &project.source_language,
                        &project.target_language,
                        &analysis.style_guide,
                        config.llm.max_retries,
                    )
                    .await
                    .map_err(|error| {
                        format!("chapter {} title failed: {error}", chapters[index].id)
                    })?
                    .remove(0);
                    apply_title(state_dir, project, chapters, index, translation)?;
                }
            }
        }
    }
    save_project_progress(state_dir, project, chapters)
}

fn apply_title(
    state_dir: &std::path::Path,
    project: &mut crate::model::ProjectState,
    chapters: &mut [Chapter],
    chapter_index: usize,
    translation: String,
) -> Result<(), String> {
    chapters[chapter_index].target_title = Some(translation.clone());
    sync_heading_title(&mut chapters[chapter_index], &translation);
    state::write_chapter(state_dir, project, &chapters[chapter_index])?;
    save_project_progress(state_dir, project, chapters)
}

fn sync_heading_title(chapter: &mut Chapter, target_title: &str) {
    for segment in &mut chapter.segments {
        if segment.kind == SegmentKind::Heading && segment.source.trim() == chapter.title.trim() {
            segment.target = Some(target_title.to_string());
            segment.status = ItemStatus::Translated;
        }
    }
}

fn chapter_body_complete(chapter: &Chapter) -> bool {
    chapter
        .segments
        .iter()
        .all(|segment| segment.status == ItemStatus::Translated && segment.target.is_some())
}

fn save_project_progress(
    state_dir: &std::path::Path,
    project: &mut crate::model::ProjectState,
    chapters: &[Chapter],
) -> Result<(), String> {
    project.chapters_completed = chapters
        .iter()
        .filter(|chapter| chapter_body_complete(chapter))
        .count();
    project.updated_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    project.status = if project.chapters_completed == project.chapters_total
        && chapters
            .iter()
            .all(|chapter| chapter.target_title.is_some())
    {
        ProjectStatus::Translated
    } else {
        ProjectStatus::Translating
    };
    state::save_project(state_dir, project)
}

fn load_translation_analysis(
    state_dir: &std::path::Path,
    project: &crate::model::ProjectState,
    chapters: &[Chapter],
    config: &AppConfig,
) -> Result<analysis::BookAnalysis, String> {
    let path = state::project_dir(state_dir, &project.id).join("analysis.json");
    if !path.is_file() {
        return Err(format!(
            "translation analysis is missing at {}; run init first",
            path.display()
        ));
    }
    let value: analysis::BookAnalysis = state::read_json(&path)?;
    if config.analysis.full_book {
        if value
            .book_synopsis
            .as_deref()
            .is_none_or(|synopsis| synopsis.trim().is_empty())
        {
            return Err("book synopsis is missing; run init first".to_string());
        }
        if let Some(chapter) = chapters.iter().find(|chapter| {
            chapter
                .meta
                .get("source_digest")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|digest| digest.trim().is_empty())
        }) {
            return Err(format!(
                "source digest is missing for chapter {}; run init first",
                chapter.id
            ));
        }
    }
    Ok(value)
}

async fn retry_pending_extractions<C: TranslationClient + ?Sized>(
    client: &C,
    store: &TermStore,
    state_dir: &std::path::Path,
    project: &crate::model::ProjectState,
    chapters: &mut [Chapter],
    max_retries: usize,
    recent_context_chars: usize,
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
        let before_segment = if extraction.batch_key == "__chapter__" {
            chapters[chapter_index].segments.len()
        } else {
            extraction
                .batch_key
                .split('|')
                .filter_map(|id| {
                    chapters[chapter_index]
                        .segments
                        .iter()
                        .position(|segment| segment.id == id)
                })
                .max()
                .map(|index| index + 1)
                .unwrap_or_default()
        };
        pipeline::write_context(
            &state::project_dir(state_dir, &project.id),
            chapters,
            chapter_index,
            before_segment,
            recent_context_chars,
        )?;
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
    let extracted = terms::extract_terms_resilient(
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

fn build_client(
    config: &AppConfig,
    mock: bool,
    state_dir: &std::path::Path,
    project: &crate::model::ProjectState,
) -> Result<Box<dyn TranslationClient>, String> {
    let recorder = UsageRecorder::new(
        &state::project_dir(state_dir, &project.id),
        &config.llm.model,
    );
    let inner: Box<dyn TranslationClient> = if mock {
        Box::new(MockClient)
    } else {
        let client = RigClient::from_config(&config.llm).map_err(|error| {
            let _ = recorder.record_event(
                "configuration_failed",
                serde_json::json!({"stage":"configuration","error":error}),
            );
            error
        })?;
        Box::new(client.with_recorder(recorder.clone()))
    };
    Ok(Box::new(RecordingClient::new(inner, recorder)))
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
    use super::{
        export_file, import_project, run_transit, transit, Cli, Command, ExportArgs,
        ExportFormatArg, TransitArgs,
    };
    use crate::analysis::BookAnalysis;
    use crate::config::AppConfig;
    use crate::llm::{CompletionOutput, MockClient, TranslationClient};
    use crate::model::{
        Document, DocumentMetadata, ItemStatus, ProjectStatus, Segment, SegmentKind,
    };
    use crate::terms::TermStore;
    use crate::usage::UsageFile;
    use crate::{parser, state};
    use async_trait::async_trait;
    use clap::Parser;
    use rig_core::completion::Usage;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "transitpls-cli-{}-{nonce}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
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

    #[test]
    fn imports_book_without_running_model_analysis() {
        let dir = temp_dir();
        let source = dir.join("book.txt");
        let config_path = dir.join("transitpls.toml");
        let state_dir = dir.join("projects");
        fs::write(&source, "Alice arrived.").unwrap();
        fs::write(
            &config_path,
            format!("[paths]\nstate_dir = {:?}\n", state_dir),
        )
        .unwrap();

        let (project, created) = import_project(Some(config_path.clone()), source.clone()).unwrap();
        assert!(created);
        assert!(!state::project_dir(&state_dir, &project.id)
            .join("analysis.json")
            .exists());

        let (_, created_again) = import_project(Some(config_path), source).unwrap();
        assert!(!created_again);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn initialization_analysis_failure_is_written_to_project_log() {
        let dir = temp_dir();
        let source = dir.join("book.txt");
        let state_dir = dir.join("projects");
        fs::write(&source, "Alice arrived.").unwrap();
        let document = parser::parse_document(&source, Some("en"), 1200).unwrap();
        let initialized = state::initialize(&state_dir, &source, &document, 1200).unwrap();
        let project_dir = state::project_dir(&state_dir, &initialized.project.id);
        fs::write(project_dir.join("analysis.json"), "invalid JSON").unwrap();
        let config_path = dir.join("config.toml");
        fs::write(&config_path, "[paths]\nstate_dir = 'projects'\n").unwrap();
        let error = super::initialize_project(Some(config_path), source, None, None, true, false)
            .await
            .unwrap_err();
        let log = fs::read_to_string(project_dir.join("logs.txt")).unwrap();
        let entry: serde_json::Value =
            serde_json::from_str(log.lines().last().unwrap().splitn(3, '\t').nth(2).unwrap())
                .unwrap();
        assert_eq!(entry["error"], error);
        assert!(log.contains("\tfailed\t"));
        assert_eq!(
            state::load_project(&state_dir, &initialized.project.id)
                .unwrap()
                .status,
            crate::model::ProjectStatus::Failed
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn accepts_zero_based_chapter_filter() {
        let cli = Cli::try_parse_from([
            "transitpls-cli",
            "transit",
            "book.txt",
            "--chapter",
            "0",
            "--mock",
        ])
        .expect("CLI arguments should parse");
        let Command::Transit(args) = cli.command else {
            panic!("transit command should parse");
        };
        assert_eq!(args.chapter, Some(0));
        assert!(args.mock);
    }

    #[test]
    fn accepts_export_format_and_output_path() {
        let cli = Cli::try_parse_from([
            "transitpls-cli",
            "export",
            "--format",
            "epub",
            "--out",
            "dist/book.epub",
            "book.txt",
        ])
        .expect("export arguments should parse");
        let Command::Export(args) = cli.command else {
            panic!("export command should parse");
        };
        assert_eq!(args.format, ExportFormatArg::Epub);
        assert_eq!(args.out, Some(PathBuf::from("dist/book.epub")));
        assert_eq!(args.input, PathBuf::from("book.txt"));
    }

    #[test]
    fn txt_export_overwrites_output_without_changing_project_log() {
        let dir = temp_dir();
        let source = dir.join("book.txt");
        let state_dir = dir.join("projects");
        fs::write(&source, "Chapter 1\n\nHello, world!").expect("source should be written");
        let document =
            parser::parse_document(&source, Some("en"), 1_200).expect("document should parse");
        let initialized = state::initialize(&state_dir, &source, &document, 1_200)
            .expect("project should initialize");
        let mut project = initialized.project;
        let mut chapters =
            state::load_chapters(&state_dir, &project).expect("chapters should load");
        chapters[0].target_title = Some("第一章".to_string());
        chapters[0].segments[0].target = Some("你好, 世界!".to_string());
        chapters[0].segments[0].status = ItemStatus::Translated;
        let lock = state::acquire_project_lock(&state_dir, &project)
            .expect("project lock should be created");
        state::save_progress(&state_dir, &mut project, &mut chapters)
            .expect("translation state should save");
        drop(lock);
        let project_dir = state::project_dir(&state_dir, &project.id);
        let log_path = project_dir.join("logs.txt");
        let log_before = fs::read(&log_path).expect("log should be readable");
        let output = dir.join("output/book.zh.txt");
        fs::create_dir_all(output.parent().expect("output should have parent"))
            .expect("output directory should be created");
        fs::write(&output, "old output").expect("old output should be written");

        let result = export_file(
            ExportArgs {
                format: ExportFormatArg::Txt,
                out: None,
                input: source,
            },
            &state_dir,
        )
        .expect("TXT export should succeed");

        assert_eq!(result, 0);
        assert_eq!(
            fs::read_to_string(output).expect("output should be readable"),
            "第一章\n\n你好， 世界！\n"
        );
        assert_eq!(
            fs::read(&log_path).expect("log should remain readable"),
            log_before
        );
        fs::remove_dir_all(dir).expect("temp directory should be removed");
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
        let mut project = initialized.project.clone();
        let mut chapters =
            state::load_chapters(&state_dir, &project).expect("chapters should load");
        crate::analysis::prepare(
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

        transit(
            TransitArgs {
                input: source.clone(),
                chapter: None,
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
        assert!(chapters[0].target_title.is_some());
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
        let project_dir = state::project_dir(&state_dir, &initialized.project.id);
        assert!(project_dir.join("context.json").is_file());
        let usage: UsageFile =
            state::read_json(&project_dir.join("usage.json")).expect("usage should load");
        assert!(usage.calls.iter().any(|entry| entry.stage == "translation"));
        assert!(usage
            .calls
            .iter()
            .any(|entry| entry.stage == "title_translation"));

        let first_target = chapters[0].segments[0].target.clone();
        let call_count = usage.calls.len();
        transit(
            TransitArgs {
                input: source.clone(),
                chapter: None,
                mock: true,
            },
            &state_dir,
            &AppConfig::default(),
        )
        .await
        .expect("completed transit should be resumable");
        let chapters =
            state::load_chapters(&state_dir, &initialized.project).expect("chapters should reload");
        let usage: UsageFile =
            state::read_json(&project_dir.join("usage.json")).expect("usage should reload");
        assert_eq!(chapters[0].segments[0].target, first_target);
        assert_eq!(usage.calls.len(), call_count);
        fs::remove_dir_all(dir).expect("temp directory should be removed");
    }

    #[tokio::test]
    async fn mock_polish_preserves_draft_and_saves_polished_target() {
        let dir = temp_dir();
        let source = dir.join("book.txt");
        let state_dir = dir.join("projects");
        fs::write(&source, "Chapter 1\n\nAlice arrived.").expect("source should be written");
        let document =
            parser::parse_document(&source, Some("en"), 1_200).expect("document should parse");
        let initialized = state::initialize(&state_dir, &source, &document, 1_200)
            .expect("project should initialize");
        let mut project = initialized.project.clone();
        let mut chapters =
            state::load_chapters(&state_dir, &project).expect("chapters should load");
        crate::analysis::prepare(
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
        let mut config = AppConfig::default();
        config.pipeline.polish = true;

        transit(
            TransitArgs {
                input: source,
                chapter: None,
                mock: true,
            },
            &state_dir,
            &config,
        )
        .await
        .expect("polished transit should complete");

        let chapters =
            state::load_chapters(&state_dir, &initialized.project).expect("chapters should reload");
        let segment = &chapters[0].segments[0];
        assert!(segment
            .target_before_polish
            .as_deref()
            .is_some_and(|value| value.starts_with("[mock zh-CN]")));
        assert!(segment
            .target
            .as_deref()
            .is_some_and(|value| value.starts_with("[mock polished zh-CN]")));
        fs::remove_dir_all(dir).expect("temp directory should be removed");
    }

    struct BatchRejectingClient {
        translation_sizes: Arc<Mutex<Vec<usize>>>,
    }

    #[async_trait]
    impl TranslationClient for BatchRejectingClient {
        async fn complete(
            &self,
            system_prompt: &str,
            user_prompt: &str,
        ) -> Result<CompletionOutput, String> {
            if system_prompt.contains("TASK:TERM_EXTRACTION") {
                return Ok(output(r#"{"terms":[]}"#.to_string()));
            }
            let value: serde_json::Value =
                serde_json::from_str(user_prompt).expect("prompt should be JSON");
            let segments = value["segments"]
                .as_array()
                .expect("prompt should contain segments");
            if system_prompt.contains("TASK:TRANSLATION") {
                self.translation_sizes
                    .lock()
                    .expect("sizes mutex")
                    .push(segments.len());
                if segments.len() > 1 {
                    return Ok(output("not json".to_string()));
                }
            }
            Ok(output(
                serde_json::json!({
                    "translations": segments.iter().enumerate().map(|(index, segment)| {
                        serde_json::json!({
                            "number": index + 1,
                            "id": segment["id"],
                            "translation": format!("translated {}", segment["source"].as_str().unwrap_or_default()),
                        })
                    }).collect::<Vec<_>>()
                })
                .to_string(),
            ))
        }
    }

    fn output(text: String) -> CompletionOutput {
        CompletionOutput {
            text,
            usage: Usage::default(),
        }
    }

    #[tokio::test]
    async fn failed_batch_falls_back_to_individual_segments() {
        let dir = temp_dir();
        let source = dir.join("book.txt");
        fs::write(&source, "book").expect("source should be written");
        let state_dir = dir.join("projects");
        let document = Document {
            metadata: DocumentMetadata {
                title: "Book".to_string(),
                source_language: "en".to_string(),
                target_language: "zh-CN".to_string(),
                source_format: "txt".to_string(),
            },
            chapters: vec![crate::model::Chapter {
                id: "chapter-1".to_string(),
                title: "Opening".to_string(),
                target_title: None,
                status: ItemStatus::Pending,
                meta: serde_json::json!({ "source_digest": "digest" }),
                segments: vec![
                    test_segment("one", "First paragraph."),
                    test_segment("two", "Second paragraph."),
                ],
            }],
        };
        let initialized = state::initialize(&state_dir, &source, &document, 1_200)
            .expect("project should initialize");
        let mut project = initialized.project;
        let mut chapters =
            state::load_chapters(&state_dir, &project).expect("chapters should load");
        let store = TermStore::open(state::project_dir(&state_dir, &project.id).join("terms.db"))
            .expect("term store should open");
        let sizes = Arc::new(Mutex::new(Vec::new()));
        let client = BatchRejectingClient {
            translation_sizes: Arc::clone(&sizes),
        };
        let analysis = BookAnalysis {
            genre: "fiction".to_string(),
            tone: "neutral".to_string(),
            style_guide: vec!["Natural Chinese".to_string()],
            narration: "third person".to_string(),
            pacing: "steady".to_string(),
            register: "neutral".to_string(),
            dialogue_style: "plain".to_string(),
            rhetoric: "plain".to_string(),
            characters: Vec::new(),
            terms: Vec::new(),
            book_synopsis: Some("synopsis".to_string()),
        };
        let mut config = AppConfig::default();
        config.llm.max_retries = 0;

        run_transit(
            &client,
            &store,
            &state_dir,
            &mut project,
            &mut chapters,
            &analysis,
            &config,
            None,
        )
        .await
        .expect("individual fallback should complete");

        assert_eq!(*sizes.lock().expect("sizes mutex"), vec![2, 1, 1]);
        assert!(chapters[0]
            .segments
            .iter()
            .all(|segment| segment.status == ItemStatus::Translated));
        assert_eq!(project.status, ProjectStatus::Translated);
        fs::remove_dir_all(dir).expect("temp directory should be removed");
    }

    #[tokio::test]
    async fn selected_chapter_leaves_other_chapters_untouched() {
        let dir = temp_dir();
        let source = dir.join("book.txt");
        fs::write(&source, "book").expect("source should be written");
        let state_dir = dir.join("projects");
        let document = Document {
            metadata: DocumentMetadata {
                title: "Book".to_string(),
                source_language: "en".to_string(),
                target_language: "zh-CN".to_string(),
                source_format: "txt".to_string(),
            },
            chapters: vec![
                crate::model::Chapter {
                    id: "chapter-1".to_string(),
                    title: "First".to_string(),
                    target_title: None,
                    status: ItemStatus::Pending,
                    meta: serde_json::json!({ "source_digest": "first digest" }),
                    segments: vec![test_segment("one", "First paragraph.")],
                },
                crate::model::Chapter {
                    id: "chapter-2".to_string(),
                    title: "Second".to_string(),
                    target_title: None,
                    status: ItemStatus::Pending,
                    meta: serde_json::json!({ "source_digest": "second digest" }),
                    segments: vec![test_segment("two", "Second paragraph.")],
                },
            ],
        };
        let initialized = state::initialize(&state_dir, &source, &document, 1_200)
            .expect("project should initialize");
        let mut project = initialized.project;
        let mut chapters =
            state::load_chapters(&state_dir, &project).expect("chapters should load");
        let store = TermStore::open(state::project_dir(&state_dir, &project.id).join("terms.db"))
            .expect("term store should open");

        run_transit(
            &MockClient,
            &store,
            &state_dir,
            &mut project,
            &mut chapters,
            &test_analysis(),
            &AppConfig::default(),
            Some(1),
        )
        .await
        .expect("selected chapter should translate");

        assert!(chapters[0].segments[0].target.is_none());
        assert!(chapters[1].segments[0].target.is_some());
        assert!(chapters
            .iter()
            .all(|chapter| chapter.target_title.is_none()));
        assert_eq!(project.chapters_completed, 1);
        assert_eq!(project.status, ProjectStatus::Translating);
        fs::remove_dir_all(dir).expect("temp directory should be removed");
    }

    fn test_analysis() -> BookAnalysis {
        BookAnalysis {
            genre: "fiction".to_string(),
            tone: "neutral".to_string(),
            style_guide: vec!["Natural Chinese".to_string()],
            narration: "third person".to_string(),
            pacing: "steady".to_string(),
            register: "neutral".to_string(),
            dialogue_style: "plain".to_string(),
            rhetoric: "plain".to_string(),
            characters: Vec::new(),
            terms: Vec::new(),
            book_synopsis: Some("synopsis".to_string()),
        }
    }

    fn test_segment(id: &str, source: &str) -> Segment {
        Segment {
            id: id.to_string(),
            ordinal: 0,
            source: source.to_string(),
            target: None,
            target_before_polish: None,
            kind: SegmentKind::Paragraph,
            status: ItemStatus::Pending,
            source_hash: "hash".to_string(),
            meta: serde_json::json!({}),
        }
    }
}
