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
`analysis.json`, and `terms.db`.

`init` accepts `--source-language` and `--max-segment-chars`. With source language
set to `auto`, the configured model identifies it and reports an explicit override
instruction if identification fails. Initialization samples the beginning, middle,
and end for style analysis, optionally creates resumable chapter digests and a book
synopsis, and seeds the terminology database. `--force-analysis` refreshes cached
analysis. `--mock` produces deterministic offline analysis and translations.

Translation extracts terminology after every saved batch and again at chapter end.
An extraction failure preserves the translated text and a pending extraction record;
the next run repairs pending terminology before translating more text. Only terms
matched in the current chapter are added to a translation prompt. Conflicting targets
are retained for manual resolution instead of overwriting an existing translation.

## Configuration

The CLI reads `transitpls.toml` in the current directory when present. Use global
`--config <path>` to require another file. Relative `paths.state_dir` values are
resolved from the configuration file's directory. CLI options override TOML values,
which override built-in defaults. See `transitpls.toml.example` for all fields.

API keys are never read directly from TOML. `llm.api_key_env` names the environment
variable that contains the secret, for example:

```text
export OPENAI_API_KEY="..."
```

Supported providers are `openai-chat`, `openai-responses`, and `anthropic`.
Configured timeouts and retry counts apply to model-backed operations.

The Tauri desktop shell remains available for the future frontend implementation.
