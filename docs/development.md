# gcloud-switch development

How gcloud-switch is built, how it is put together, and how its documentation and releases are published. The user manual is the [README](../README.md).

## Build and run

```bash
cargo install --path .     # Install from source
cargo build                # Debug build
cargo build --release      # Release build
cargo run                  # Build and run the TUI
cargo run -- list          # Run a subcommand
cargo check                # Quick type-check without building
cargo test                 # Run unit tests
cargo clippy               # Lint
cargo fmt                  # Format code
```

Unit tests live in `#[cfg(test)]` modules beside the code they cover: the table column geometry in `src/ui.rs`; quota-project stamping, credential handling, the activation message, the live-state descriptions and the rule that matches the live ADC to a profile in `src/gcloud.rs`; the `list` table and the `status` report in `src/main.rs`. Everything without a test, which is most of the TUI and all gcloud interaction, is verified by building and running the relevant command against a throwaway profile.

## Architecture

Seven modules with clear separation:

- **main.rs** — CLI parsing (clap) and TUI lifecycle. Subcommands: `add`, `list`, `status`, `activate`, `authenticate`, `import`, `sync`, `self-update`, `open-readme`, `completion`, `show-config`, or no subcommand for the interactive TUI. The global `--html-help` flag opens the documentation site at the invoked command's section. Handles TUI suspend/resume when spawning interactive gcloud auth commands.
- **app.rs** — Core state machine. Manages `InputMode` (Normal, Edit, AddProfile, ConfirmDelete), profile selection, background auth checking, edit suggestions, and pending actions. The `Column` enum (Both, User, ADC) selects the `Parts` that activate, authenticate and edit act on; `active_adc` tracks whose stored ADC credential is live; `live` is the `LiveState` read from gcloud's files, with the validity of its two credentials checked in the background like the profiles'.
- **ui.rs** — Ratatui rendering. Layout is 5 rows: the two live-state lines, table, status bar, help line (which carries the title). Renders inline editing with cursor positioning and dropdown suggestion overlays.
- **gcloud.rs** — All gcloud CLI and OAuth2 integration, and the activation/authentication core shared by TUI and CLI (`Parts`, `activate`, `authenticate`, `parts_needing_auth`, `active_adc_profile`, `live_state`). Manages configurations via gcloud CLI commands, queries `credentials.db` (SQLite, read-only) for OAuth tokens, validates user and stored ADC tokens via Google's token endpoint, and spawns `gcloud auth login` / `gcloud auth application-default login`.
- **store.rs** — Persistent storage in `~/.config/gcloud/gcloud-switch/`. Profiles stored as TOML, ADC credentials as JSON files per profile (mode 0600).
- **profile.rs** — Data structures: `Profile` (user_account, user_project, adc_account, adc_quota_project; the projects may be empty), `ProfilesFile`, `SyncMode`.
- **sync.rs** — Git-based sync of `profiles.toml` through the system `git` CLI; the newer `updated_at` wins per profile.

## Key design decisions

- Auth validation runs on background threads (not tokio tasks) because `rusqlite` and `reqwest::blocking` would conflict with the tokio runtime. User checks are deduplicated by account; ADC checks run per profile against the stored credential file.
- TUI must suspend (restore terminal, drop alternate screen) before spawning interactive gcloud commands, then resume after. The `PendingAction` enum defers actions that require TUI suspension until the main loop can handle them outside the event handler. Errors from a pending action go to the status bar; the TUI never quits on them.
- Profile activation uses the gcloud CLI (`gcloud config set ... --configuration=<name>`, `gcloud config configurations activate`) so gcloud's internal state stays consistent. The ADC file is the only direct file operation (no gcloud CLI equivalent exists): the stored per-profile credential, stamped with the profile's quota project, is written to the live ADC path. `gcloud auth application-default set-quota-project` is not used because it edits only the live file, which may belong to another profile.
- ADC validity is checked against the stored per-profile credential, not against the ADC account's user credential, so an ADC account that never ran `gcloud auth login` is not shown as expired.
- `gcloud auth login` runs with `--no-activate`, so authenticating a profile never switches gcloud's active configuration. Both logins are given the profile's account, because that is gcloud's own account check: after the browser flow gcloud compares the signed-in account with the argument and refuses a different one. Given an account, gcloud also skips an ADC login whenever the live ADC file already names that account, valid or not, so the live file is moved aside for the ADC login and put back when the login fails. Every stored ADC names its account (gcloud records it after that check; gcloud-switch records it where it writes the credential itself).
- Both logins run with `--verbosity=error`. gcloud's warnings describe the state before gcloud-switch finishes; the "Quota project is disabled" warning of an ADC login with `--disable-quota-project` is printed a moment before the quota project is stamped in.
- A profile with one account for both parts gets its ADC without a browser when the user credential is valid: the ADC is built from that credential in `credentials.db`, the same document `gcloud auth login --update-adc` writes. When both parts need a login, one login with `--update-adc` serves both. Such profiles share one refresh token, so the live ADC is matched to a profile by refresh token and quota project, preferring the active profile; a revoked user login (`gcloud auth revoke`) takes the derived ADCs with it.
- Global settings that exist but cannot be read or parsed are an error. They used to reset to defaults silently, which the next save wrote back over the user's file.

## Documentation site

The site at <https://tjirsch.github.io/gcloud-switch/> is rendered from this repository's Markdown; nothing is written for it separately. `README.md` becomes `index.html`, and every `docs/*.md` named in `SITE_DOCS` of `scripts/build-site.py` becomes `docs/<name>.html`. The build fails on a doc that is in neither `SITE_DOCS` nor `SITE_DOCS_EXCLUDED`, on a page missing from `NAV_ORDER`, on a link to a Markdown file that does not exist, and on a link to an anchor that does not exist. Headings get GitHub's own ids, so an anchor works on GitHub and on the site alike. A heading of the form `### Activate a profile (`activate`)` also gets `id="cmd-activate"`, which is what `gcloud-switch activate --html-help` opens.

`scripts/build-doc.py` renders one Markdown file with cmark-gfm, GitHub's parser, into a self-contained themed page (light and dark). `scripts/build-site.py` assembles the pages with the shared navigation bar, the "On this page" column and the search box. Both are uv scripts that declare their dependency. Render locally before a documentation change ships:

```bash
uv run scripts/build-site.py _site
open _site/index.html
```

`.github/workflows/pages.yml` publishes `_site/` to GitHub Pages on every release tag and on manual dispatch (Actions → docs site → Run workflow). `gcloud-switch open-readme`, `--html-help` and the post-install step of `self-update` open the site.

## Release

A release adds its section to `CHANGELOG.md` first (Keep a Changelog form, one `## [X.Y.Z] - date` section per version): cargo-dist reads that section into the GitHub release notes. Releases are built by cargo-dist (`dist-workspace.toml`, `.github/workflows/release.yml`) when a `vX.Y.Z` tag is pushed: binaries for macOS (Intel, Apple Silicon) and Linux (x86_64, ARM64) plus the `gcloud-switch-installer.sh` installer. `.github/workflows/attach-checksum.yml` adds the `gcloud-switch-installer.sh.sha256` sidecar that `self-update` verifies before running the installer. The same tag triggers the documentation site build.

## Dependencies

Key crates: `ratatui` + `crossterm` (TUI), `clap` (CLI), `reqwest` (HTTP for token validation and the release API), `rusqlite` with bundled SQLite (credentials.db access), `serde` + `toml` + `serde_json` (serialization), `sha2` + `hex` (installer checksum), `anyhow` (error handling).
