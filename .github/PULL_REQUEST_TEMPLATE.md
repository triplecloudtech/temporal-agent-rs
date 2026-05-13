<!--
Thanks for sending a PR! Please fill in the sections below. Skip the
"Test plan" only for pure-doc / non-code changes.
-->

## Summary

<!-- 1-2 sentences. What does this change do? -->

## Motivation

<!-- Why are we making this change? Link issues if any: "Closes #123". -->

## Test plan

<!-- How did you verify this works? -->

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cargo test --lib`
- [ ] `cargo test --test '*' -- --include-ignored` (Docker required)
- [ ] `cargo doc --no-deps`

## Checklist

- [ ] CHANGELOG updated under `## [Unreleased]` if user-visible
- [ ] Public API changes documented with `///` doc comments
- [ ] No new `unsafe` (forbidden by lints)
- [ ] Workflow code remains deterministic (see [AGENTS.md](../AGENTS.md))
