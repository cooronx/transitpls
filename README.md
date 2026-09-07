# TransItPls

TransItPls is a long-form document translation workflow. The Rust CLI parses EPUB
and TXT books, creates content-addressed project state, prepares whole-book analysis,
maintains a live terminology database, and translates batches through Rig.

## CLI

Build and run the CLI from `src-tauri`:

```text
cargo run --bin transitpls-cli -- init book.epub
cargo run --bin transitpls-cli -- init book.epub --mock
cargo run --bin transitpls-cli -- init book.epub --mock --force-analysis
cargo run --bin transitpls-cli -- status book.epub
cargo run --bin transitpls-cli -- status --project <source-sha256>
cargo run --bin transitpls-cli -- transit book.epub --mock
cargo run --bin transitpls-cli -- transit book.epub --chapter 0 --mock
cargo run --bin transitpls-cli -- terms list book.epub
cargo run --bin transitpls-cli -- terms conflicts book.epub
cargo run --bin transitpls-cli -- terms resolve book.epub "source" "fixed target"
cargo run --bin transitpls-cli -- terms list --project <source-sha256>
cargo run --bin transitpls-cli -- review book.epub
cargo run --bin transitpls-cli -- export --format epub book.epub
```

Projects are stored under `projects/<source-sha256>/` by default. Initialization is
built under `projects/.creating/` and atomically moved into place after parsing is
complete. Each project contains `project.json`, chapter JSON files, `logs.txt`,
`analysis.json`, `terms.db`, and translation-time `context.json`, `usage.json`, and
`project.lock` files.

`init` accepts `--source-language` and `--max-segment-chars`. With source language
set to `auto`, the configured model identifies it and reports an explicit override
instruction if identification fails. Initialization samples the beginning, middle,
and end for style analysis, optionally creates resumable chapter digests and a book
synopsis, and discovers terminology candidates. `--force-analysis` refreshes cached
analysis. `--mock` produces deterministic offline analysis and translations.

Translation batches are sized by source character count and run serially in book
order. Prompts include the style guide, book synopsis, chapter digest, relevant
terminology, and recent translated text. Each completed batch is saved atomically;
rerunning the command rebuilds context without replacing completed translations.
After a batch exhausts its retries, the CLI retries each segment individually and
stops at the first segment that still fails. Use `--chapter <zero-based-index>` to
translate one chapter while debugging a long book.

Translation extracts terminology after every saved batch and again at chapter end.
An extraction failure preserves the translated text and a pending extraction record;
the next run repairs pending terminology before translating more text. Only terms
confirmed by a reviewer and matched in the current chapter are added to a translation
prompt. Conflicting targets are retained as candidates instead of overwriting a
confirmed translation.
After every chapter body is complete, chapter titles are translated and matching body
headings are synchronized for later table-of-contents export.

Set `pipeline.polish = true` to polish each translated batch. The initial translation
is retained in `target_before_polish`, while the final text is stored in `target`.
`pipeline.recent_context_chars` controls the rolling context character budget. Model
calls and cumulative token counts are stored in `usage.json`; pricing and cost
estimation are intentionally not supported. An operating-system file lock prevents
concurrent commands from modifying the same project.

## Terminology candidates

Before model analysis, a complete-source scan counts repeated 1-5 grams. It uses
words for spaced scripts and characters for Chinese, Japanese, and Korean, folds
case and width, and stores normalized NFC text alongside surface forms, occurrence
counts, first/last chapters, and up to three local contexts. N-grams do not cross
punctuation, paragraph, or chapter boundaries. They are suggestions, not automatic
glossary entries.

The terminology workspace provides **Scan Full Text**, search, evidence inspection,
confirmation, rejection, and assignment to an existing confirmed term as a variant.
Equivalent CLI commands (run from `src-tauri`) are:

```text
cargo run --bin transitpls-cli -- terms scan --project <id>
cargo run --bin transitpls-cli -- terms candidates --project <id>
cargo run --bin transitpls-cli -- terms review --project <id> --normalized alice --status confirmed --target "爱丽丝"
```

Use `--proposed-target` to select an LLM proposal with a nonempty proposed target.
Use `--drift` to select a translation-drift record. `--status rejected` dismisses a
candidate; `--status variant --target <confirmed-source>` attaches an alias. Reviewed
records retain their decisions across rescans. Edit an established translation with
the existing `terms resolve` command or the terminology table.

Candidate states are `candidate`, `confirmed`, `rejected`, and `variant`; provenance
supports `llm`, `frequency`, `ner`, and `translation-drift`. NER integration is not
included. Existing glossary rows remain compatible: `resolved` means confirmed,
while legacy `ok` and `conflict` rows require manual confirmation before injection.

LLM extraction uses strict records containing `source`, `category`, `variants`,
`evidence_count`, `contexts`, `proposed_target`, `confidence`, and `reason`. Low
confidence and empty proposed translations are allowed; invalid evidence and
unexpected fields are rejected. Pretranslation extraction samples up to 12,000
characters per chapter and caches results separately from style analysis. The
complete-source frequency candidates remain available outside those samples.

Translation, polishing, and title prompts carry a separate `confirmed_terms`
constraint. After saved translation batches, missing confirmed targets and observed
alternative translations create drift candidates without changing the glossary.
Missing-target checks indicate possible omission or drift; they do not infer an
unobserved replacement or prove segment alignment.

## Configuration

The CLI reads `transitpls.toml` in the current directory when present. Use global
`--config <path>` to require another file. Relative `paths.state_dir` values are
resolved from the configuration file's directory. CLI options override TOML values,
which override built-in defaults. See `transitpls.toml.example` for all fields.

API keys are never written to TOML or project state. Desktop users can enter and
verify a key once under **Settings → Model & API**; it is stored in the current
user's TransItPls configuration directory. CLI and automation users can instead
set the environment variable named by `llm.api_key_env`, for example:

```text
export OPENAI_API_KEY="..."
```

Supported providers are `openai-chat`, `openai-responses`, and `anthropic`.
Configured timeouts and retry counts apply to model-backed operations.

## Desktop app

Run the desktop app from the repository root:

```text
yarn tauri dev
```

The desktop workbench reads the same project directories and configuration as the
CLI. It can import EPUB/TXT books, display chapter and segment progress, run or
cancel translation tasks, inspect and resolve terminology conflicts, configure and
verify model credentials, and export translated TXT or EPUB files. Enable offline
simulation in the task inspector to exercise the workflow without an API key.

The Review workspace is intentionally a non-functional placeholder until the
stage 8 review and reporting core is implemented.
