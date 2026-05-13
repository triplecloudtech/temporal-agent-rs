# Security Policy

## Supported versions

`temporal-agent-rs` is pre-1.0. Security fixes will land on the
**latest 0.x** release line only. Older 0.x lines are not maintained.

| Version  | Supported |
| -------- | --------- |
| `0.1.x`  | ✅        |

## Reporting a vulnerability

Please **do not** open a public GitHub issue for security problems.

Use GitHub's private vulnerability reporting:

1. Go to the
   [Security tab](https://github.com/triplecloudtech/temporal-agent-rs/security/advisories)
   of this repository.
2. Click **Report a vulnerability**.
3. Fill in what you found, including a minimal reproduction if possible.

We aim to acknowledge reports within **72 hours** and to ship a fix or
mitigation within **14 days** for high-severity issues. Lower-severity
issues may take longer.

If GitHub's private reporting is not available for any reason, please
contact the maintainers via the address in
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) — note that this is **not** a
preferred channel for vulnerabilities; the GitHub flow above is faster
and more confidential.

## Scope

In scope:

- This crate's source code and published artifacts on crates.io.
- The examples directory, to the extent they reflect recommended usage
  patterns.

Out of scope (please report upstream):

- Vulnerabilities in [`temporalio-*`](https://github.com/temporalio/sdk-core)
  Rust SDK crates.
- Vulnerabilities in [`autoagents`](https://github.com/liquidos-ai/AutoAgents).
- Vulnerabilities in third-party LLM providers (OpenAI, Anthropic,
  Ollama, etc.).

## Disclosure

Once a fix is available we will publish a coordinated GitHub Security
Advisory and a `cargo audit` advisory where appropriate, then ship a
patched release to crates.io.
