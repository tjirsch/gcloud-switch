# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

```sh
cargo build              # debug build
cargo build --release    # release build
cargo run                # build and run the TUI
cargo run -- list        # run a specific subcommand
cargo check              # quick type-check
cargo test               # unit tests
cargo clippy             # lint
cargo fmt                # format
uv run scripts/build-site.py _site   # render the documentation site into _site/
```

Unit tests live in `#[cfg(test)]` modules beside the code they cover (`src/ui.rs`: the table column geometry that places the edit cursor and the suggestion dropdown); `cargo test` runs them. Everything without a test — most of the TUI and all gcloud interaction — is verified by building (`cargo build`) and running the relevant command.

## Architecture

Rust CLI + TUI app for switching between Google Cloud configurations. Seven modules in `src/`:

- **main.rs** — CLI parsing (clap with derive), global settings (`~/.config/gcloud-switch/gcloud-switch.toml`), self-update logic, and the TUI lifecycle. All subcommand dispatch happens here (`add`, `list`, `status`, `activate`, `authenticate`, `import`, `sync`, `self-update`, `open-readme`, `completion`, `show-config`). `open_url()` opens the documentation site (`DOCS_URL`) in the browser: after `self-update`, for `open-readme`, and for the global `--html-help` flag, which `open_html_help()` points at the invoked command's `#cmd-<name>` section.
- **app.rs** — TUI state machine. Manages `InputMode` enum (Normal, Edit, AddProfile, ConfirmDelete), profile selection, background auth checking via `mpsc` channels, edit suggestions, and `PendingAction` for deferring operations that require TUI suspension (interactive gcloud auth). `Column` (Both, User, Adc) maps to `gcloud::Parts` and scopes activate, authenticate and edit to one part; `active_adc` is the profile whose stored ADC credential is live; `live` is the `LiveState` read from gcloud's files, its two credentials checked in the background like the profiles'.
- **ui.rs** — Ratatui rendering. Layout: two live-state lines, profile table, status bar, help line (with the title). Handles inline editing with cursor and dropdown suggestion overlays. Active styling is per part cell.
- **gcloud.rs** — All gcloud CLI interaction and OAuth2 token validation, plus the activation/authentication core shared by TUI and CLI: `Parts`, `write_configuration`, `activate`, `activation_message`, `authenticate`, `authenticate_only`, `parts_needing_auth`, `active_adc_profile`, `live_state`. Reads `credentials.db` (SQLite, read-only) for user tokens, validates user and stored ADC tokens via Google's token endpoint, spawns interactive `gcloud auth login` / `gcloud auth application-default login`.
- **store.rs** — Persistent storage under `~/.config/gcloud/gcloud-switch/`. Profiles in TOML, ADC credentials as JSON files per profile, written with mode 0600 (`write_adc_file`).
- **profile.rs** — Data types: `Profile`, `ProfilesFile`, `SyncMode`. Accounts are required; `user_project` and `adc_quota_project` may be empty (empty = none).
- **sync.rs** — Git-based profile sync using system `git` CLI. Merge strategy: newer `updated_at` timestamp wins per profile.

## Key Design Patterns

- Auth validation runs on **std::thread** (not tokio) because `rusqlite` and `reqwest::blocking` would conflict with a tokio runtime. User checks are deduplicated by account email; ADC checks run per profile against the stored credential file.
- TUI must **suspend** (restore terminal, leave alternate screen) before spawning interactive gcloud commands, then resume. The `PendingAction` enum defers these until the main loop can handle them outside the event handler; `ReauthAndActivate` carries the `Parts` that need a login. Errors from a pending action go to the status bar; the TUI never quits on them.
- The stored per-profile ADC file is the **source of truth** for that profile's ADC. Activation stamps the profile's quota project into it and writes it to the live ADC path; `gcloud auth application-default set-quota-project` is never used because it edits only the live file. gcloud configuration properties are always written with `--configuration=<name>`, so edits and activation do not depend on the active configuration. `gcloud auth login` runs with `--no-activate` so authenticating never switches the active configuration.
- Both logins are given the profile's account (gcloud verifies the browser signed in as it and refuses otherwise) and run with `--verbosity=error` (gcloud's warnings describe the file before the stamp). The live ADC file is moved aside during an ADC login, because gcloud skips a login for an account the live file already names, valid or not. Every stored ADC names its account. A profile with one account for both parts gets its ADC from the valid user credential in `credentials.db` without a browser (the document `gcloud auth login --update-adc` writes), and one `--update-adc` login when both parts need one; such profiles share a refresh token, so `active_adc_profile` matches on token and quota project and prefers the active profile.
- Two separate config locations: **global settings** in `~/.config/gcloud-switch/gcloud-switch.toml` (update frequency, sync remote) and **profile data** in `~/.config/gcloud/gcloud-switch/profiles.toml`. A settings file that exists but cannot be read or parsed is an error, never silently replaced.
- `BTreeMap` is used for profiles to maintain stable alphabetical ordering.

## Documentation

Docs ship with the change, in two renderings: the Markdown in the repo (`README.md`, `docs/*.md`) and the HTML site <https://tjirsch.github.io/gcloud-switch/> rendered from exactly that Markdown by `scripts/build-site.py` (renderer: `scripts/build-doc.py`, cmark-gfm, GitHub's parser). Nothing is written for the site separately. Every `docs/*.md` must be named in the script's `SITE_DOCS` or `SITE_DOCS_EXCLUDED` (with its reason), and every page in `NAV_ORDER`; the build fails otherwise, as it does on a link to a missing file or anchor. `.github/workflows/pages.yml` publishes the site on every release tag and on manual dispatch. Run `uv run scripts/build-site.py _site` before a docs change ships and open `_site/index.html`. `README.md` is the user manual; `docs/development.md` holds architecture, design decisions and the pipelines. `CHANGELOG.md` (Keep a Changelog form) gets a `## [X.Y.Z] - date` section with every release, before the release: cargo-dist reads it into the GitHub release notes. No editor integration exists: the program opens URLs in the browser only.

## Adding CLI Subcommands

1. Add a variant to the `Commands` enum in `main.rs` (clap derive)
2. Add the match arm in `main()`
3. If the command shouldn't trigger update checks, add it to the `!matches!()` guard
4. Add a `### <Title> (`<cmd>`)` section to `README.md` (the site turns it into `#cmd-<cmd>`) and the command to `DOCUMENTED` in `open_html_help()`, so `gcloud-switch <cmd> --html-help` opens it
