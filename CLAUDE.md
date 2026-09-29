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
```

Unit tests live in `#[cfg(test)]` modules beside the code they cover (`src/ui.rs`: the table column geometry that places the edit cursor and the suggestion dropdown); `cargo test` runs them. Everything without a test — most of the TUI and all gcloud interaction — is verified by building (`cargo build`) and running the relevant command.

## Architecture

Rust CLI + TUI app for switching between Google Cloud configurations. Seven modules in `src/`:

- **main.rs** — CLI parsing (clap with derive), global settings (`~/.config/gcloud-switch/gcloud-switch.toml`), self-update logic, and the TUI lifecycle. All subcommand dispatch happens here (`add`, `list`, `activate`, `authenticate`, `import`, `sync`, `self-update`, `open-readme`, `completion`, `set-editor`, `show-config`, `edit-config`). The `open_file()` function implements editor resolution: configured editor → `$EDITOR` → OS default.
- **app.rs** — TUI state machine. Manages `InputMode` enum (Normal, Edit, AddProfile, ConfirmDelete), profile selection, background auth checking via `mpsc` channels, edit suggestions, and `PendingAction` for deferring operations that require TUI suspension (interactive gcloud auth). `Column` (Both, User, Adc) maps to `gcloud::Parts` and scopes activate, authenticate and edit to one part; `active_adc` is the profile whose stored ADC credential is live.
- **ui.rs** — Ratatui rendering. Layout: title bar, profile table, status bar, help line. Handles inline editing with cursor and dropdown suggestion overlays. Active styling is per part cell.
- **gcloud.rs** — All gcloud CLI interaction and OAuth2 token validation, plus the activation/authentication core shared by TUI and CLI: `Parts`, `write_configuration`, `activate`, `authenticate`, `authenticate_only`, `parts_needing_auth`, `active_adc_profile`. Reads `credentials.db` (SQLite, read-only) for user tokens, validates user and stored ADC tokens via Google's token endpoint, spawns interactive `gcloud auth login` / `gcloud auth application-default login`.
- **store.rs** — Persistent storage under `~/.config/gcloud/gcloud-switch/`. Profiles in TOML, ADC credentials as JSON files per profile, written with mode 0600 (`write_adc_file`).
- **profile.rs** — Data types: `Profile`, `ProfilesFile`, `SyncMode`. Accounts are required; `user_project` and `adc_quota_project` may be empty (empty = none).
- **sync.rs** — Git-based profile sync using system `git` CLI. Merge strategy: newer `updated_at` timestamp wins per profile.

## Key Design Patterns

- Auth validation runs on **std::thread** (not tokio) because `rusqlite` and `reqwest::blocking` would conflict with a tokio runtime. User checks are deduplicated by account email; ADC checks run per profile against the stored credential file.
- TUI must **suspend** (restore terminal, leave alternate screen) before spawning interactive gcloud commands, then resume. The `PendingAction` enum defers these until the main loop can handle them outside the event handler; `ReauthAndActivate` carries the `Parts` that need a login. Errors from a pending action go to the status bar; the TUI never quits on them.
- The stored per-profile ADC file is the **source of truth** for that profile's ADC. Activation stamps the profile's quota project into it and writes it to the live ADC path; `gcloud auth application-default set-quota-project` is never used because it edits only the live file. gcloud configuration properties are always written with `--configuration=<name>`, so edits and activation do not depend on the active configuration. `gcloud auth login` runs with `--no-activate` so authenticating never switches the active configuration.
- Two separate config locations: **global settings** in `~/.config/gcloud-switch/gcloud-switch.toml` (update frequency, editor, sync remote) and **profile data** in `~/.config/gcloud/gcloud-switch/profiles.toml`.
- `BTreeMap` is used for profiles to maintain stable alphabetical ordering.

## Adding CLI Subcommands

1. Add a variant to the `Commands` enum in `main.rs` (clap derive)
2. Add the match arm in `main()`
3. If the command shouldn't trigger update checks, add it to the `!matches!()` guard
4. Update `README.md` with usage examples
