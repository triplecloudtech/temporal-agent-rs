# Releasing

This crate is published to [crates.io](https://crates.io/crates/temporal-agent-rs)
via a tag-driven GitHub Actions workflow
([.github/workflows/release.yml](.github/workflows/release.yml)).
Pushing a tag of the form `v*` to the repo triggers the workflow, which:

1. Asserts the tag matches `Cargo.toml`'s `package.version`.
2. Runs `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --lib`,
   and `cargo doc -D warnings` against the tagged commit.
3. Runs `cargo publish --dry-run` then `cargo publish`.
4. Creates a GitHub Release whose body is the matching `## [X.Y.Z]`
   section of [CHANGELOG.md](CHANGELOG.md). Tags containing a `-`
   (SemVer prerelease suffix) are marked **Pre-release** automatically.

## One-time setup

A repository administrator must add a single secret before the first release:

- **`CARGO_REGISTRY_TOKEN`** — a crates.io API token from
  <https://crates.io/settings/tokens>.
  - First release: leave the scope unrestricted so the token can claim the
    `temporal-agent-rs` name (`publish-new` is required for a fresh crate).
  - After the first release: rotate to a token scoped to `publish-update`
    on the `temporal-agent-rs` crate only.

Add it under **Settings → Secrets and variables → Actions → New repository
secret** in GitHub.

## Stable release (`vX.Y.Z`)

1. Bump `version` in [Cargo.toml](Cargo.toml).
2. In [CHANGELOG.md](CHANGELOG.md), move everything under `## [Unreleased]`
   into a new `## [X.Y.Z] - YYYY-MM-DD` section and update the link
   footers at the bottom of the file.
3. Run `cargo build` to refresh `Cargo.lock`.
4. Commit as `chore(release): X.Y.Z`, open a PR, merge after CI is green.
5. After merge to `main`:
   ```bash
   git checkout main
   git pull
   git tag -a vX.Y.Z -m "vX.Y.Z"
   git push origin vX.Y.Z
   ```
6. Watch the **Release** workflow in the
   [Actions tab](https://github.com/triplecloudtech/temporal-agent-rs/actions).
   On success:
   - The crate appears at <https://crates.io/crates/temporal-agent-rs>.
   - docs.rs builds within ~30 minutes.
   - A GitHub Release at
     `https://github.com/triplecloudtech/temporal-agent-rs/releases/tag/vX.Y.Z`
     contains the CHANGELOG section.

## Release candidate (`vX.Y.Z-rc.N`, also `-alpha.N` / `-beta.N`)

The same procedure as a stable release, with these differences:

- **Cargo.toml** must use a valid SemVer prerelease string:
  `version = "0.2.0-rc.1"`. Use lowercase identifiers and a dot before the
  counter — `-rc.1`, not `-rc1` or `-RC.1`.
- **CHANGELOG.md** gets a dedicated section per RC, e.g.
  `## [0.2.0-rc.1] - 2026-06-10`. Do not reuse the eventual `## [0.2.0]`
  heading — each RC needs its own section so the GitHub Release for that
  RC has meaningful notes.
- **Tag** uses the same `v` prefix: `git tag -a v0.2.0-rc.1 -m "v0.2.0-rc.1"`.
- **Iterate** the counter for each candidate: `-rc.1` → `-rc.2` → … → final.

### How consumers see RCs

Cargo's default resolver **skips prerelease versions**. A downstream
project with `temporal-agent-rs = "0.1"` will never auto-upgrade to
`0.2.0-rc.1`. Testers opt in explicitly:

```toml
temporal-agent-rs = "=0.2.0-rc.1"
```

The GitHub Release is automatically marked Pre-release (badge in the UI,
excluded from the "Latest release" pointer). docs.rs builds the RC at
`docs.rs/temporal-agent-rs/0.2.0-rc.1/`; the `/latest` redirect stays on
the highest stable version.

### SemVer prerelease ordering

`alpha < beta < rc < final`, with numeric identifiers sorted numerically:

```text
0.2.0-alpha.1 < 0.2.0-beta.1 < 0.2.0-rc.1 < 0.2.0-rc.2 < 0.2.0-rc.10 < 0.2.0
```

## Promoting RC to stable

Once the final RC has soaked:

1. Bump `Cargo.toml` from `0.2.0-rc.3` to `0.2.0` (drop the suffix —
   `0.2.0` is a new version on crates.io, not a rename of any RC).
2. Add a new `## [0.2.0] - YYYY-MM-DD` section in
   [CHANGELOG.md](CHANGELOG.md) above the RC sections. Either summarize
   the RCs ("Includes everything from 0.2.0-rc.1 through 0.2.0-rc.3
   plus: …") or consolidate the change list. Leave the RC sections in
   place for traceability.
3. Tag and push `v0.2.0` as for any stable release.

## Yanking a release

If a published version has a security or correctness regression that
warrants pulling it from the resolver, yank it:

```bash
cargo yank --version X.Y.Z
```

This works for stable releases and RCs. Existing `Cargo.lock` files
continue to resolve the yanked version; only new resolutions are
blocked. Reserve yanks for genuine bugs — don't yank for documentation
typos or minor cosmetic issues.

## Troubleshooting the workflow

| Failure | Likely cause | Fix |
|---------|--------------|-----|
| Verify-tag step fails | Tag and `Cargo.toml` version disagree | Update one to match, re-tag |
| `cargo publish --dry-run` fails on metadata | Missing/invalid field in `Cargo.toml` | Fix the field, re-tag with a new patch version |
| `cargo publish` fails with "already published" | Tag was re-pushed for an already-published version | Bump version, re-tag |
| CHANGELOG-extract step fails | No `## [X.Y.Z]` section for the tag | Add the section, re-tag (or run `gh release create` manually from the existing tag) |
| GitHub Release step fails after publish succeeded | Network blip or token scope | Re-run the failed step in the workflow UI, or run `gh release create vX.Y.Z --notes-file RELEASE_NOTES.md` locally |

If you need to recover from a partial release (crate published, GitHub
Release not created), the easiest fix is to create the GitHub Release
manually from the existing tag:

```bash
# Extract the CHANGELOG section the workflow would have used
version="X.Y.Z"
awk -v ver="$version" '
  BEGIN { hf = "## [" ver "] "; ho = "## [" ver "]" }
  substr($0, 1, length(hf)) == hf || $0 == ho { capture = 1; next }
  /^## \[/ { capture = 0 }
  /^\[[^]]+\]:/ { capture = 0 }
  capture { print }
' CHANGELOG.md > /tmp/notes.md

gh release create "v${version}" --notes-file /tmp/notes.md
# Add --prerelease if the version contains a "-" suffix
```

Re-running the whole workflow won't help — `cargo publish` will fail
with "already published" and abort the job before reaching the GitHub
Release step.
