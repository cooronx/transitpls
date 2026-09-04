# TransItPls

TransItPls is a long-form document translation workflow. The current implementation
provides the Rust CLI foundation for EPUB/TXT parsing, durable project state, and
batch translation through Rig.

## CLI

Build and run the CLI from `src-tauri`:

```text
cargo run --bin transitpls-cli -- init book.epub
cargo run --bin transitpls-cli -- status book.epub
cargo run --bin transitpls-cli -- transit book.epub --mock
cargo run --bin transitpls-cli -- review book.epub
cargo run --bin transitpls-cli -- export --format epub book.epub
```

Projects are stored under `projects/<sha256>/` in the process working directory.
`init` accepts `--source-language` and `--max-segment-chars`; the default target
language is `zh-CN` and the default segment limit is 2000 Unicode characters.

Real translation reads `transitpls.toml` (override with global `--config`) using the
schema in `transitpls.toml.example`. Supported providers are `openai-chat`,
`openai-responses`, and `anthropic`. Timeout is 60 seconds and failed batches are
retried three times with exponential backoff.

The Tauri desktop shell remains available for the future frontend implementation.
