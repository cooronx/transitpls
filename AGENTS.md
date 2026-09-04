# Project Collaboration Rules

## Git Commits

- Use the Conventional Commits format, such as `feat: add glossary storage` or `fix: handle empty input`.
- Keep the commit message subject concise and in English. The complete subject must not exceed 70 characters.
- After completing a small feature or fix that compiles successfully, create a separate commit. Do not accumulate unrelated changes into one large commit.
- Before committing, inspect `git status` and `git diff`. Commit only changes made for the current task; do not overwrite or include unrelated user changes.
- Do not create commits without meaningful changes. Keep documentation, configuration, and refactoring changes scoped clearly.

## Rust Project Validation

- Run `cargo fmt` after every change to a Rust project.
- Run `cargo clippy` after every change to a Rust project, and address warnings related to the current change.
- If the project provides dedicated build, check, or test commands, follow the existing project commands first.
- Before committing, confirm that the project compiles successfully. If a check cannot be run, explain the reason in the final result.

## Tests

- Tests should cover real behavior and important edge cases; do not add mechanical tests for every file.
- Prefer a small number of focused, maintainable unit tests. Avoid testing the same implementation detail repeatedly.
- Add tests for public interfaces, core logic, and fixes that are likely to regress. Tests are not required for simple type changes or renames.

## Comments

- Add comments only for critical logic, non-obvious constraints, compatibility handling, or code that is easy to modify incorrectly.
- Comments should explain the reason, constraint, or intent rather than repeat what the code already says.
- Use common, clear technical terminology. Do not invent terms that cannot be understood outside the project.
- Keep comments synchronized with the code and remove comments that are no longer applicable.
