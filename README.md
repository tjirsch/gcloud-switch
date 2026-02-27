# gcloud-switch

A TUI (Terminal User Interface) tool for managing and switching between multiple Google Cloud configurations. Quickly switch gcloud user credentials and Application Default Credentials (ADC) across different projects and accounts. Keeps gcloud and gcloud-switch data in sync.

Fun and learning project of mine from serveral aspects: Rust, OSS, Public Repo, AI.

## Features

- Interactive TUI for browsing and activating profiles
- Manages both **user credentials** (`gcloud auth`) and **ADC** (`gcloud auth application-default`) per profile
- Auto-detects expired tokens and triggers re-authentication before activation
- Visual auth status indicators (🔑 valid / 🔒 expired) per profile
- Import existing gcloud configurations
- CLI subcommands for scripting
- Configurable sync with gcloud configurations (strict, add-only, or off)

## Installation

Requires a working `gcloud` CLI installation.

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/tjirsch/rs-gcloud-switch/releases/latest/download/gcloud-switch-installer.sh | sh
```

Prebuilt binaries can also be downloaded directly from the [Releases](https://github.com/tjirsch/rs-gcloud-switch/releases) page (macOS Intel + Apple Silicon, Linux x86_64 + ARM64).

### macOS: "zsh: killed" error

macOS Gatekeeper quarantines unsigned binaries downloaded from the internet. To fix this:

**CLI:**
```sh
xattr -d com.apple.quarantine /usr/local/bin/gcloud-switch
```

**GUI:** Right-click the binary in Finder, select **Open**, then confirm in the dialog. Alternatively, go to **System Settings > Privacy & Security** and click **Allow Anyway** after the first blocked attempt.

## Usage

### TUI (default)

```sh
gcloud-switch
```

Opens an interactive table of profiles. Use the keyboard to navigate and activate.

### Key Bindings

| Key | Action |
|-----|--------|
| `Down` | Move selection down |
| `Up` | Move selection up |
| `Left` | Move column left (Both -> User) |
| `Right` | Move column right (User -> ADC) |
| `Enter` | Activate selected profile(s) and quit |
| `Alt+Enter` | Activate selected profile(s) |
| `a` | Re-authenticate selected profile(s) |
| `e` | Edit selected profile in-place |
| `n` | Add a new profile |
| `d` | Delete selected profile |
| `Esc` | Quit |

#### Edit Mode

| Key | Action |
|-----|--------|
| Type | Modify the field value directly in the table cell |
| `Down` | Open suggestion dropdown (known accounts or projects) |
| `Up` / `Down` | Navigate suggestions |
| `Enter` | Pick suggestion (if dropdown open) or save and exit |
| `Tab` | Move from account field to project field; save from project |
| `Esc` | Cancel edit without saving |

Suggestions include all account emails from existing profiles plus all authenticated accounts from gcloud's credential store. Project suggestions also include GCP projects accessible by the entered account.

#### Add Profile

When adding a profile (`n`), you are prompted for: profile name, user account, user project, ADC account, then ADC quota project. Prompts show defaults in **brackets**, e.g. `Enter ADC quota project [my-project]:`. **Press Enter with no input to accept the value in brackets** (ADC account and quota project default to the user account and project you just entered). Type a different value and press Enter to override.

### Column Selection

- **Both** (default): Activates both user config and ADC together
- **User**: Activates only the gcloud user configuration (account + project)
- **ADC**: Activates only the Application Default Credentials

### Sync Modes

Press `s` in the TUI to cycle through sync modes. The current mode is shown in the help bar.

| Mode | Behavior |
|------|----------|
| **strict** | Bidirectional sync — creating or deleting a profile also creates or deletes the corresponding gcloud configuration |
| **add** | One-way — new profiles create gcloud configurations, but deleting a profile does not remove the gcloud configuration |
| **off** | No sync — gcloud configurations are not touched |

The sync mode is persisted across sessions.

### CLI Subcommands

```sh
# Add a profile
gcloud-switch add myprofile --account user@example.com --project my-project

# Add with separate ADC settings
gcloud-switch add myprofile \
  --account user@example.com \
  --project my-project \
  --adc-account other@example.com \
  --adc-quota-project other-project

# List all profiles
gcloud-switch list

# Switch to a profile (non-interactive)
gcloud-switch switch myprofile

# Import existing gcloud configurations
gcloud-switch import

# Check for and install a new release from GitHub (runs the same installer as curl)
gcloud-switch self-update

# Only check if an update is available (no install, no README)
gcloud-switch self-update --check-only

# Skip downloading README after install, or skip opening it
gcloud-switch self-update --no-download-readme
gcloud-switch self-update --no-download-readme --no-open-readme

# Download and open the latest README
gcloud-switch open-readme

# Generate shell completion script
gcloud-switch completion bash >> ~/.bash_completion
gcloud-switch completion zsh --install    # installs to ~/.zsh/completions/_gcloud-switch
gcloud-switch completion                  # macOS: defaults to zsh --install

# Set editor for opening files
gcloud-switch set-editor code
gcloud-switch set-editor --clear   # revert to $EDITOR / OS default
gcloud-switch set-editor           # show current setting
```

**Self-update options:** `--no-download-readme`, `--no-open-readme`, `--check-only`. The program can also check for updates automatically when you run other commands; this is controlled by the [configuration file](#configuration-configgcloud-switchgcloud-switchtoml) `~/.config/gcloud-switch/gcloud-switch.toml` (`self_update_frequency`: `never`, `always`, or `daily`).

`self-update` compares the current version with the latest GitHub release; if an update is available it downloads and runs the installer script, then optionally downloads the README to your Downloads folder and opens it. The editor used to open the README follows the priority: `editor` config → `$EDITOR` env var → OS default app.

### Shell Completion

Generate tab-completion for your shell (`bash`, `zsh`, `fish`, `powershell`):

```sh
# Print to stdout
gcloud-switch completion bash
gcloud-switch completion zsh

# Auto-install to the canonical shell location
gcloud-switch completion zsh --install
# → installs to ~/.zsh/completions/_gcloud-switch
# → prints fpath setup instructions

gcloud-switch completion fish --install
# → installs to ~/.config/fish/completions/gcloud-switch.fish
```

On macOS, running `gcloud-switch completion` without arguments defaults to `zsh --install`.

**Install locations for `--install`:**

| Shell | Path |
|-------|------|
| bash | `~/.local/share/bash-completion/completions/gcloud-switch` |
| zsh | `~/.zsh/completions/_gcloud-switch` |
| fish | `~/.config/fish/completions/gcloud-switch.fish` |
| powershell | `%USERPROFILE%\Documents\PowerShell\Completions\gcloud-switch.ps1` |

For zsh, add this to `~/.zshrc` if not already present:
```zsh
fpath=(~/.zsh/completions $fpath)
autoload -Uz compinit && compinit
```

### Sync profiles via Git (optional)

You can sync profile **metadata only** (profile names, account and project IDs) between machines using your own Git remote (e.g. a private GitHub repo). No credentials or tokens are ever synced; each machine keeps its own `gcloud auth` state.

1. **One-time setup:** set the remote URL (and optional branch):
   ```sh
   gcloud-switch sync init https://github.com/you/your-repo.git
   gcloud-switch sync init https://github.com/you/your-repo.git --branch main
   ```
   This writes `~/.config/gcloud/gcloud-switch/sync-config.toml` and clones the repo into `sync-repo/` under that directory. Use SSH or HTTPS; auth is your normal git config (SSH keys or credential helper).

2. **Push** current profiles to the remote:
   ```sh
   gcloud-switch sync push
   ```

3. **Pull** and merge from the remote (newer profile wins per profile; new remote profiles are added; if the same profile changed on both sides you are prompted to keep local or remote):
   ```sh
   gcloud-switch sync pull
   ```

Merge is done profile-by-profile using an `updated_at` timestamp: the newer version wins. If both sides have the same timestamp and different content, the CLI prompts **Keep (L)ocal or (R)emote?**.

## Configuration (~/.config/gcloud-switch/gcloud-switch.toml)

User-level **parameters** (e.g. when to check for updates, editor) live in **`~/.config/gcloud-switch/gcloud-switch.toml`**. This file is **created on first run** with default values (e.g. `self_update_frequency = "always"`). The folder `~/.config/gcloud-switch/` may already exist (e.g. installer leaves `gcloud-switch-receipt.json` there); the program creates it if needed and writes `gcloud-switch.toml` there.

Example:

```toml
self_update_frequency = "daily"
editor = "zed"
```

Use `gcloud-switch set-editor <editor>` to set the editor from the command line.

| Option | Default | Description |
|--------|---------|-------------|
| `self_update_frequency` | `"always"` | When to check for updates on normal runs: `never`, `always`, or `daily` (at most once per 24 hours). The check is check-only (no install, no README). |
| `editor` | *(none)* | Editor command used to open files (e.g. `"zed"`, `"code"`, `"vim"`). Falls back to `$EDITOR` env var, then the OS default app. |

**Profile data** stays in **`profiles.toml`** under `~/.config/gcloud/gcloud-switch/` (see [File Locations](#file-locations)); it is not stored in `~/.config/gcloud-switch/`.

## Data Flow

### Profile Storage

Profiles are stored in `~/.config/gcloud/gcloud-switch/profiles.toml`:

```toml
[profiles.myprofile]
user_account = "user@example.com"
user_project = "my-project"
adc_account = "user@example.com"
adc_quota_project = "my-project"
```

### Activation

When a profile is activated:

1. **User config**: A gcloud configuration is created (if needed) and activated via `gcloud config configurations activate`, then account and project are set via `gcloud config set`
2. **ADC**: The stored ADC JSON is copied to `~/.config/gcloud/application_default_credentials.json`

### Auth Validation

On startup, gcloud-switch reads `~/.config/gcloud/credentials.db` (a SQLite database maintained by gcloud) to look up stored OAuth2 credentials for each profile's account. It then performs a token refresh request to validate whether the credentials are still valid. The result is shown as a lock indicator:

- 🔑 Token is valid, profile can be activated immediately
- 🔒 Token is expired or missing, re-authentication will be triggered on activation

### Re-authentication

When activating a profile with an invalid token, gcloud-switch automatically runs:
- `gcloud auth login --account=<email>` for user credentials
- `gcloud auth application-default login` for ADC credentials

When activating with the column set to **Both**, this results in two separate browser-based auth dialogs — one for user credentials and one for ADC.

You can also manually trigger re-auth with the `a` key.

## File Locations

| Path | Description |
|------|-------------|
| `~/.config/gcloud-switch/gcloud-switch.toml` | User parameters (`self_update_frequency`, `editor`). Created on first run with defaults. |
| `~/.config/gcloud/gcloud-switch/profiles.toml` | Profile definitions |
| `~/.config/gcloud/gcloud-switch/sync-config.toml` | Optional Git sync config (remote URL, branch) |
| `~/.config/gcloud/gcloud-switch/sync-repo/` | Git clone used for sync (profiles.toml only) |
| `~/.config/gcloud/gcloud-switch/adc/<name>.json` | Stored ADC credentials per profile |
| `~/.config/gcloud/credentials.db` | gcloud's OAuth2 credential store (read-only) |
| `~/.config/gcloud/configurations/` | gcloud configuration files (written on activate) |
| `~/.config/gcloud/active_config` | gcloud's active configuration pointer |
| `~/.config/gcloud/application_default_credentials.json` | Active ADC file |

## License

MIT

## Development

```bash
cargo install --path .     # Install from source
cargo build                # Debug build
cargo build --release      # Release build
cargo run                  # Build and run the TUI
cargo check                # Quick type-check without building
cargo clippy               # Lint
cargo fmt                  # Format code
```

### Architecture

Six modules with clear separation:

- **main.rs** — CLI parsing (clap) and TUI lifecycle. Subcommands: `add`, `list`, `switch`, `import`, or no subcommand for interactive TUI. Handles TUI suspend/resume when spawning interactive gcloud auth commands.
- **app.rs** — Core state machine. Manages `InputMode` (Normal, Edit, AddProfile, ConfirmDelete), profile selection, background auth checking, edit suggestions, and pending actions. The `Column` enum controls whether activation targets both user+ADC, user-only, or ADC-only credentials.
- **ui.rs** — Ratatui rendering. Layout is 4 rows: title, table, status bar, help line. Renders inline editing with cursor positioning and dropdown suggestion overlays.
- **gcloud.rs** — All gcloud CLI and OAuth2 integration. Manages configurations via gcloud CLI commands, queries `credentials.db` (SQLite, read-only) for OAuth tokens, validates tokens via Google's token endpoint, and spawns `gcloud auth login` / `gcloud auth application-default login`.
- **store.rs** — Persistent storage in `~/.config/gcloud/gcloud-switch/`. Profiles stored as TOML, ADC credentials as JSON files per profile.
- **profile.rs** — Data structures: `Profile` (user_account, user_project, adc_account, adc_quota_project), `ProfilesFile`, `StateFile`.

### Key Design Decisions

- Auth validation runs on background threads (not tokio tasks) because `rusqlite` and `reqwest::blocking` would conflict with the tokio runtime. Auth checks are deduplicated by account.
- TUI must suspend (restore terminal, drop alternate screen) before spawning interactive gcloud commands, then resume after.
- `PendingAction` enum defers actions that require TUI suspension until the main loop can handle them outside the event handler.
- Profile activation uses gcloud CLI (`gcloud config configurations activate`, `gcloud config set`) to ensure gcloud's internal state stays consistent. ADC file copy is the only direct file operation (no gcloud CLI equivalent exists).

### Dependencies

Key crates: `ratatui` + `crossterm` (TUI), `clap` (CLI), `reqwest` (HTTP for token validation), `rusqlite` with bundled SQLite (credentials.db access), `serde` + `toml` + `serde_json` (serialization), `anyhow` (error handling).
