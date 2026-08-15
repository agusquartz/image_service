# Contributing to Image Service

Thanks for your interest in contributing to Image Service.

## Development Setup

Clone the repository and verify that the project builds:

```bash
cargo build
```

For local development:

```bash
cargo run
```

## Before Committing

Format the Rust sources:

```bash
cargo fmt --all
```

Then review your changes:

```bash
git status
git diff
```

## Before Pushing or Opening a Pull Request

Run the same checks used by GitHub Actions:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo build --release --locked
```

All commands should succeed before changes are pushed or submitted for review.

### What These Checks Do

- `cargo fmt --all -- --check` verifies that all Rust source files follow the project's formatting rules.
- `cargo clippy --all-targets --all-features --locked -- -D warnings` runs Clippy against all targets and treats warnings as errors.
- `cargo test --workspace --all-features --locked` runs the complete test suite.
- `cargo build --release --locked` verifies that the optimized production binary builds using the committed dependency lockfile.

The `--locked` flag ensures that Cargo uses the dependency versions recorded in `Cargo.lock` and fails if the lockfile would need to change.

These checks are also automatically executed by the repository's GitHub Actions CI workflow.

## Git Workflow

Keep commits focused and descriptive.

Examples:

```text
fix: improve atomic image storage
feat: add image deletion
test: add namespace validation tests
docs: update deployment guide
ci: update Rust quality checks
refactor: separate storage backend
```

Before pushing:

```bash
git status
git log --oneline --decorate -5
git push
```

`git push` itself is not part of the quality checks; run it only after the checks above pass.

## Pull Requests

Please:

- Keep changes focused on one problem or feature.
- Explain why the change is needed.
- Add or update tests when behavior changes.
- Update documentation when API, configuration, or deployment behavior changes.
- Avoid unrelated refactoring in the same pull request.
- Make sure CI passes.

## Security

Do not report security vulnerabilities through public issues.

See [SECURITY.md](SECURITY.md).

## Code of Conduct

Participation in this project is governed by the
[Code of Conduct](CODE_OF_CONDUCT.md).
