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

Unit tests live in `#[cfg(test)]` modules beside the code they cover: the table column geometry in `src/ui.rs`, quota-project stamping and credential handling in `src/gcloud.rs`, the `list` table in `src/main.rs`. Everything without a test, which is most of the TUI and all gcloud interaction, is verified by building and running the relevant command against a throwaway profile.

## Architecture

Seven modules with clear separation:

- **main.rs** — CLI parsing (clap) and TUI lifecycle. Subcommands: `add`, `list`, `activate`, `authenticate`, `import`, `sync`, `self-update`, `open-readme`, `completion`, `show-config`, or no subcommand for the interactive TUI. The global `--html-help` flag opens the documentation site at the invoked command's section. Handles TUI suspend/resume when spawning interactive gcloud auth commands.
- **app.rs** — Core state machine. Manages `InputMode` (Normal, Edit, AddProfile, ConfirmDelete), profile selection, background auth checking, edit suggestions, and pending actions. The `Column` enum (Both, User, ADC) selects the `Parts` that activate, authenticate and edit act on; `active_adc` tracks whose stored ADC credential is live.
- **ui.rs** — Ratatui rendering. Layout is 4 rows: title, table, status bar, help line. Renders inline editing with cursor positioning and dropdown suggestion overlays.
- **gcloud.rs** — All gcloud CLI and OAuth2 integration, and the activation/authentication core shared by TUI and CLI (`Parts`, `activate`, `authenticate`, `parts_needing_auth`, `active_adc_profile`). Manages configurations via gcloud CLI commands, queries `credentials.db` (SQLite, read-only) for OAuth tokens, validates user and stored ADC tokens via Google's token endpoint, and spawns `gcloud auth login` / `gcloud auth application-default login`.
- **store.rs** — Persistent storage in `~/.config/gcloud/gcloud-switch/`. Profiles stored as TOML, ADC credentials as JSON files per profile (mode 0600).
- **profile.rs** — Data structures: `Profile` (user_account, user_project, adc_account, adc_quota_project; the projects may be empty), `ProfilesFile`, `SyncMode`.
- **sync.rs** — Git-based sync of `profiles.toml` through the system `git` CLI; the newer `updated_at` wins per profile.

## Key design decisions

- Auth validation runs on background threads (not tokio tasks) because `rusqlite` and `reqwest::blocking` would conflict with the tokio runtime. User checks are deduplicated by account; ADC checks run per profile against the stored credential file.
- TUI must suspend (restore terminal, drop alternate screen) before spawning interactive gcloud commands, then resume after. The `PendingAction` enum defers actions that require TUI suspension until the main loop can handle them outside the event handler. Errors from a pending action go to the status bar; the TUI never quits on them.
- Profile activation uses the gcloud CLI (`gcloud config set ... --configuration=<name>`, `gcloud config configurations activate`) so gcloud's internal state stays consistent. The ADC file is the only direct file operation (no gcloud CLI equivalent exists): the stored per-profile credential, stamped with the profile's quota project, is written to the live ADC path. `gcloud auth application-default set-quota-project` is not used because it edits only the live file, which may belong to another profile.
- ADC validity is checked against the stored per-profile credential, not against the ADC account's user credential, so an ADC account that never ran `gcloud auth login` is not shown as expired.
- `gcloud auth login` runs with `--no-activate`, so authenticating a profile never switches gcloud's active configuration. The ADC login is run without an account argument on purpose: given one, gcloud skips the login whenever the live ADC file already names that account, even when that credential is expired. gcloud-switch checks the signed-in account itself and discards a login for the wrong account.
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

Releases are built by cargo-dist (`dist-workspace.toml`, `.github/workflows/release.yml`) when a `vX.Y.Z` tag is pushed: binaries for macOS (Intel, Apple Silicon) and Linux (x86_64, ARM64) plus the `gcloud-switch-installer.sh` installer. `.github/workflows/attach-checksum.yml` adds the `gcloud-switch-installer.sh.sha256` sidecar that `self-update` verifies before running the installer. The same tag triggers the documentation site build.

## Dependencies

Key crates: `ratatui` + `crossterm` (TUI), `clap` (CLI), `reqwest` (HTTP for token validation and the release API), `rusqlite` with bundled SQLite (credentials.db access), `serde` + `toml` + `serde_json` (serialization), `sha2` + `hex` (installer checksum), `anyhow` (error handling).
