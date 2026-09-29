# gcloud-switch

> **📖 Documentation: <https://tjirsch.github.io/gcloud-switch/>** — this README and the
> [development notes](https://tjirsch.github.io/gcloud-switch/docs/development.html), searchable,
> rebuilt on every release. Also: `gcloud-switch open-readme`, or `gcloud-switch <command> --html-help`.

A TUI (Terminal User Interface) tool for managing and switching between multiple Google Cloud configurations. Quickly switch gcloud user credentials and Application Default Credentials (ADC) across different projects and accounts. Keeps gcloud and gcloud-switch data in sync.

Fun and learning project of mine from serveral aspects: Rust, OSS, Public Repo, AI.

## Features

- Interactive TUI for browsing and activating profiles
- Manages both **user credentials** (`gcloud auth`) and **ADC** (`gcloud auth application-default`) per profile; each part can be activated or authenticated on its own
- Projects are optional: a profile can have no user project and no ADC quota project
- Auto-detects expired tokens and triggers re-authentication before activation, for the affected part only
- Visual auth status indicators (🔑 valid / 🔒 expired) and active markers per part
- Import existing gcloud configurations
- CLI subcommands for scripting (`list`, `activate`, `authenticate`, `add`, ...)
- Configurable sync with gcloud configurations (strict, add-only, or off)

## Installation

Requires a working `gcloud` CLI installation.

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/tjirsch/gcloud-switch/releases/latest/download/gcloud-switch-installer.sh | sh
```

Prebuilt binaries can also be downloaded directly from the [Releases](https://github.com/tjirsch/gcloud-switch/releases) page. Builds exist for macOS (Intel, Apple Silicon) and Linux (x86_64, ARM64); there is no native Windows build, so on Windows use the Linux x86_64 build inside WSL, where `gcloud` and its `~/.config/gcloud` live as on Linux.

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
| `Enter` | Activate the selected part(s) and quit |
| `Alt+Enter` | Activate the selected part(s) and stay |
| `a` | Authenticate (log in) the selected part(s) without activating |
| `e` | Edit the selected part in-place |
| `n` | Add a new profile |
| `d` | Delete the selected profile (the whole entry) |
| `s` | Cycle the sync mode (strict, add, off) |
| `i` | Import gcloud configurations that are not profiles yet |
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

When adding a profile (`n`), you are prompted for: profile name, user account, user project, ADC account, then ADC quota project. Both accounts are required; both projects are optional (press Enter on an empty prompt for none). The ADC account prompt shows its default in **brackets**, e.g. `Enter ADC account [user@example.com]:`; press Enter with no input to accept it. The ADC quota project prompt is **prefilled** with the user project: press Enter to keep it, edit it, or delete it for no quota project.

### Column Selection

`Left`/`Right` select which part of a profile `Enter` (activate), `a` (authenticate) and `e` (edit) act on:

- **Both** (default): user configuration and ADC together
- **User**: only the gcloud user configuration (account + project)
- **ADC**: only the Application Default Credentials (account + quota project)

Active markers are shown per part: the user cell is green when that profile's configuration is gcloud's active configuration, the ADC cell when that profile's stored ADC credential is the live ADC file. The two can belong to different profiles, e.g. after activating only the ADC of another entry.

### Sync Modes

Press `s` in the TUI to cycle through sync modes. The current mode is shown in the help bar.

| Mode | Behavior |
|------|----------|
| **strict** | Bidirectional sync — creating or deleting a profile also creates or deletes the corresponding gcloud configuration |
| **add** | One-way — new profiles create gcloud configurations, but deleting a profile does not remove the gcloud configuration |
| **off** | No sync — gcloud configurations are not touched |

The sync mode is persisted across sessions.

## CLI

Every command has `--help`. `gcloud-switch <command> --html-help` opens that command's section of the [documentation site](https://tjirsch.github.io/gcloud-switch/) in the browser; `gcloud-switch --html-help` opens the front page.

| Command | Purpose |
|---------|---------|
| `add` | Add a profile |
| `list` | List all profiles as a table, with the parts that are live |
| `activate` | Activate a profile: its gcloud configuration, its ADC, or both |
| `authenticate` | Log in for a profile without activating it |
| `import` | Import existing gcloud configurations as profiles |
| `sync` | Sync `profiles.toml` through a Git remote |
| `self-update` | Install the latest release from GitHub |
| `open-readme` | Open the documentation site |
| `completion` | Generate or install shell completion |
| `show-config` | Print the global settings file |

### Add a profile (`add`)

```sh
# The project is optional
gcloud-switch add myprofile --account user@example.com --project my-project
gcloud-switch add adminprofile --account admin@example.com

# Separate ADC settings ("" = no quota project)
gcloud-switch add myprofile \
  --account user@example.com \
  --project my-project \
  --adc-account other@example.com \
  --adc-quota-project other-project
```

The ADC account defaults to the user account, the ADC quota project to the user project. Both accounts are required. In sync modes strict and add, the gcloud configuration is created as well.

### List profiles (`list`)

```sh
gcloud-switch list
```

One row per profile. `ACTIVE` shows which parts are live in gcloud: `both`, `user` (its configuration is gcloud's active one), `adc` (its stored credential is the live ADC file) or `-`. Empty fields print `-`.

### Activate a profile (`activate`)

```sh
gcloud-switch activate myprofile          # user configuration and ADC
gcloud-switch activate myprofile --user   # only the gcloud configuration
gcloud-switch activate myprofile --adc    # only the Application Default Credentials
```

The non-interactive counterpart of `Enter` in the TUI: a part whose credential is missing or expired is logged in first (see [Re-authentication](#re-authentication)), then the part is activated (see [Activation](#activation)).

### Authenticate a profile (`authenticate`)

```sh
gcloud-switch authenticate myprofile          # user credentials and ADC
gcloud-switch authenticate myprofile --user
gcloud-switch authenticate myprofile --adc
```

Logs in without activating. After an ADC login the ADC that was live before is put back, so authenticating one profile never switches another one's ADC.

### Import gcloud configurations (`import`)

```sh
gcloud-switch import
```

Creates a profile for every gcloud configuration that has an account and is not a profile yet; the configuration's account and project become both parts of the profile.

### Sync profiles via Git (`sync`)

You can sync profile **metadata only** (profile names, account and project IDs) between machines using your own Git remote (e.g. a private GitHub repo). No credentials or tokens are ever synced; each machine keeps its own `gcloud auth` state.

1. **One-time setup:** set the remote URL (and optional branch):
   ```sh
   gcloud-switch sync init https://github.com/you/your-repo.git
   gcloud-switch sync init https://github.com/you/your-repo.git --branch main
   ```
   This stores the remote and branch in `~/.config/gcloud-switch/gcloud-switch.toml` and clones the repo into `~/.config/gcloud-switch/sync-repo/`. Use SSH or HTTPS; auth is your normal git config (SSH keys or credential helper).

2. **Push** current profiles to the remote:
   ```sh
   gcloud-switch sync push
   ```

3. **Pull** and merge from the remote (newer profile wins per profile; new remote profiles are added; if the same profile changed on both sides you are prompted to keep local or remote):
   ```sh
   gcloud-switch sync pull
   ```

Merge is done profile-by-profile using an `updated_at` timestamp: the newer version wins. If both sides have the same timestamp and different content, the CLI prompts **Keep (L)ocal or (R)emote?**.

### Self-update (`self-update`)

```sh
# Check for and install a new release (same installer as curl)
gcloud-switch self-update

# Only check if an update is available (no install)
gcloud-switch self-update --check-only

# Do not open the documentation site after installing
gcloud-switch self-update --no-open-readme
```

Compares the current version with the latest GitHub release. When a newer version is available it downloads the installer script, verifies its SHA-256 checksum against the release's `.sha256` sidecar (`--skip-checksum` for a release without one), runs it, then prints the documentation URL and opens the site unless `--no-open-readme` is given.

The program can also check for updates when you run other commands; `self_update_frequency` in the [global settings](#configuration-configgcloud-switchgcloud-switchtoml) controls this (`never`, `always`, or `daily`). That check only reports; it never installs.

### Open the documentation (`open-readme`)

```sh
gcloud-switch open-readme
```

Opens the documentation site, <https://tjirsch.github.io/gcloud-switch/>, in the browser: this README and the development notes, rendered from the repository's Markdown on every release tag (`.github/workflows/pages.yml`).

### Shell completion (`completion`)

Generate tab-completion for your shell (`bash`, `zsh`, `fish`, `powershell`):

```sh
# Print to stdout
gcloud-switch completion bash
gcloud-switch completion zsh

# Auto-install to the canonical shell location
gcloud-switch completion bash --install
# → installs to ~/.local/share/bash-completion/completions/gcloud-switch

gcloud-switch completion zsh --install
# → installs to ~/.zsh/completions/_gcloud-switch

gcloud-switch completion fish --install
# → installs to ~/.config/fish/completions/gcloud-switch.fish
```

On macOS, running `gcloud-switch completion` without arguments defaults to `zsh --install`. Re-run the install after upgrading so tab completion knows the current commands (`switch` was replaced by `activate`; `set-editor` and `edit-config` are gone).

**Install locations for `--install`:**

| Shell | Path |
|-------|------|
| bash | `~/.local/share/bash-completion/completions/gcloud-switch` |
| zsh | `~/.zsh/completions/_gcloud-switch` |
| fish | `~/.config/fish/completions/gcloud-switch.fish` |
| powershell | `%USERPROFILE%\Documents\PowerShell\Completions\gcloud-switch.ps1` |

#### Bash (Ubuntu / Linux)

1. Install the `bash-completion` package if not already present:
   ```bash
   sudo apt install bash-completion
   ```

2. Install the completion script:
   ```bash
   gcloud-switch completion bash --install
   ```
   This writes the script to `~/.local/share/bash-completion/completions/gcloud-switch`.

3. Ensure `bash-completion` is sourced in your `~/.bashrc`. Most Ubuntu installations include this by default, but verify these lines exist:
   ```bash
   if [ -f /usr/share/bash-completion/bash_completion ]; then
       . /usr/share/bash-completion/bash_completion
   fi
   ```
   The `bash-completion` package automatically discovers user completions from `~/.local/share/bash-completion/completions/` — no extra `source` line is needed for the individual script.

4. Reload your shell:
   ```bash
   source ~/.bashrc
   ```

> **Note:** Do not append the raw completion output to `~/.bashrc` or `~/.bash_completion`. Using `--install` places the script in its own dedicated file under the standard completions directory, which avoids conflicts with other completion scripts.

#### Zsh (macOS)

Add this to `~/.zshrc` if not already present:
```zsh
fpath=(~/.zsh/completions $fpath)
autoload -Uz compinit && compinit
```

Then install:
```zsh
gcloud-switch completion zsh --install
```

### Show the global settings (`show-config`)

```sh
gcloud-switch show-config
```

Prints the path of the global settings file and its content. Edit the file with any editor; the options are listed under [Configuration](#configuration-configgcloud-switchgcloud-switchtoml).

## Configuration (~/.config/gcloud-switch/gcloud-switch.toml)

User-level **parameters** (when to check for updates, the Git sync remote) live in **`~/.config/gcloud-switch/gcloud-switch.toml`**. This file is **created on first run** with default values (`self_update_frequency = "always"`). The folder `~/.config/gcloud-switch/` may already exist (the installer leaves `gcloud-switch-receipt.json` there); the program creates it if needed. A file that exists but cannot be read or parsed is an error; it is never silently replaced with defaults.

Example:

```toml
self_update_frequency = "daily"
```

| Option | Default | Description |
|--------|---------|-------------|
| `self_update_frequency` | `"always"` | When to check for updates on normal runs: `never`, `always`, or `daily` (at most once per 24 hours). The check only reports; it never installs. |
| `remote_url`, `branch` | *(none)* | Git remote and branch for `sync`, written by `gcloud-switch sync init`. |
| `sync_files` | `["profiles.toml"]` | Files under `~/.config/gcloud/gcloud-switch/` that `sync push` and `sync pull` transfer. |

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

Both accounts are required. Both project fields may be empty (`""`): then no `core/project` is set on the gcloud configuration and no `quota_project_id` is written into the ADC file.

### Activation

When a profile, or one part of it, is activated:

1. **User config**: The gcloud configuration is created if needed, its account and project are written with `gcloud config set ... --configuration=<name>` (an empty project is `unset`), then it is made active via `gcloud config configurations activate`.
2. **ADC**: The stored ADC credential (`adc/<name>.json`) is stamped with the profile's quota project (`quota_project_id` set, or removed when the quota project is empty) and written to `~/.config/gcloud/application_default_credentials.json`. The stored file is the source of truth: the live file is always "stored credential + the profile's quota project". If no credential is stored yet, an ADC login runs first.

Editing a profile (`e`) applies the change at once: the user part rewrites the gcloud configuration (in sync modes strict and add), a changed quota project is stamped into the stored ADC credential and, when that credential is the live one, into the live ADC file.

### Auth Validation

On startup, gcloud-switch validates each part with a token refresh request. User credentials come from `~/.config/gcloud/credentials.db` (a SQLite database maintained by gcloud), checked once per account. ADC credentials come from the profile's stored `adc/<name>.json`; a stored file that records a different account than the profile's ADC account counts as invalid. The result is shown as a lock indicator per part:

- 🔑 Token is valid, profile can be activated immediately
- 🔒 Token is expired or missing, re-authentication will be triggered on activation

### Re-authentication

When activating a part with an invalid token, gcloud-switch first runs, for that part only:
- `gcloud auth login <email> --no-activate --force` for user credentials. `--no-activate` keeps gcloud's active configuration unchanged.
- `gcloud auth application-default login --disable-quota-project` for ADC. Sign in with the profile's ADC account: gcloud-switch checks the account of the resulting credential and discards the login (restoring the previous ADC file) if it differs. It then stamps the profile's quota project into the credential and stores it for the profile.

`a` (or `gcloud-switch authenticate`) logs in without activating. After an ADC login the ADC that was live before is put back, so authenticating one profile never switches another one's ADC.

## File Locations

| Path | Description |
|------|-------------|
| `~/.config/gcloud-switch/gcloud-switch.toml` | User parameters (`self_update_frequency`, Git sync `remote_url` and `branch`). Created on first run with defaults. |
| `~/.config/gcloud-switch/sync-repo/` | Git clone used for sync (profiles.toml only) |
| `~/.config/gcloud/gcloud-switch/profiles.toml` | Profile definitions |
| `~/.config/gcloud/gcloud-switch/adc/<name>.json` | Stored ADC credential per profile (mode 0600); the source of truth for that profile's ADC |
| `~/.config/gcloud/credentials.db` | gcloud's OAuth2 credential store (read-only) |
| `~/.config/gcloud/configurations/` | gcloud configuration files (written on add, edit and activate) |
| `~/.config/gcloud/active_config` | gcloud's active configuration pointer |
| `~/.config/gcloud/application_default_credentials.json` | Live ADC file (written on activate, mode 0600) |

## Development

```bash
cargo install --path .               # Install from source
cargo build                          # Debug build
cargo test                           # Run unit tests
cargo clippy                         # Lint
uv run scripts/build-site.py _site   # Render the documentation site into _site/
```

Architecture, design decisions, the release pipeline and how the documentation site is built: [development notes](docs/development.md).

## License

MIT
