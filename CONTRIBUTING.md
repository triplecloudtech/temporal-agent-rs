# Contributing to temporal-agent-rs

Thanks for your interest in contributing. This is a small early-stage
project, so we keep the process light.

## Local setup

You'll need:

- **Rust ≥ 1.95** — install via [rustup](https://rustup.rs). The repo pins
  the stable toolchain via `rust-toolchain.toml`, so `rustup` will fetch
  the matching components automatically on first build.
- **Temporal CLI** — for running the examples against a local dev server.
  Install with `brew install temporal` or follow
  [https://docs.temporal.io/cli#install](https://docs.temporal.io/cli#install).
- **Docker** — required only for running the integration test suite,
  which spins up Temporal and Ollama containers via
  [`testcontainers`](https://crates.io/crates/testcontainers).
- An OpenAI-compatible API key (only for running the `simple_math_agent`
  / `interactive_math_agent` examples; the integration test uses a local
  Ollama model and needs no external service).

## Build & test

```bash
# Build everything
cargo build

# Unit tests (fast; no Docker)
cargo test --lib

# Integration tests (slow; requires Docker — pulls ~400 MB Ollama model on first run)
cargo test --test '*' -- --include-ignored
```

## Style & quality

These are enforced in CI; please run them locally before pushing:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo doc --no-deps
```

We also run `cargo deny check` and `cargo audit` on a schedule and on
dependency changes. If your PR touches `Cargo.toml` or `Cargo.lock`,
please run `cargo deny check` locally.

## Pull requests

1. Fork the repo and create a topic branch off `main`.
2. Make your change. Keep PRs focused — one logical change per PR.
3. Update `CHANGELOG.md` under `## [Unreleased]` if your change is
   user-visible.
4. Push the branch and open a PR against `main`.
5. CI must be green before merge.

Commit messages: we suggest (but don't require)
[Conventional Commits](https://www.conventionalcommits.org/) — e.g.
`feat: add streaming LLM support`, `fix: handle empty tool args`.

## Releasing

The release process — version bumps, CHANGELOG curation, tagging, and
how the tag-driven publish workflow behaves for stable and prerelease
versions — is documented in [RELEASING.md](RELEASING.md).

## Determinism contract

When editing `src/workflow.rs` or `src/state.rs`, keep the determinism
contract intact: the workflow must be reproducible on replay. No
wall-clock, no random, no I/O — that all lives in
[`src/activities.rs`](src/activities.rs). See [AGENTS.md](AGENTS.md) for
the full rules.

## Reporting bugs

Use the [bug report issue
template](.github/ISSUE_TEMPLATE/bug_report.yml). Include your OS, Rust
version, Temporal CLI version, and a minimal repro.

## Reporting security issues

See [SECURITY.md](SECURITY.md). Do **not** open public issues for
security problems.

## Code of conduct

This project follows the
[Contributor Covenant](CODE_OF_CONDUCT.md). By participating, you agree
to abide by its terms.
