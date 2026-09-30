mod app;
mod gcloud;
mod profile;
mod store;
mod sync;
mod ui;

use std::io;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::builder::NonEmptyStringValueParser;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::app::{App, PendingAction};
use crate::gcloud::Parts;
use crate::profile::{Profile, ProfilesFile, SyncMode};
use crate::store::Store;

#[derive(Parser)]
#[command(name = "gcloud-switch", version, about = "TUI Google Cloud profile switcher")]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Open the documentation site in the browser at this command's section
    /// (`gcloud-switch activate --html-help`); alone, the front page
    #[arg(long, global = true)]
    html_help: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Add a new profile
    Add {
        /// Profile name
        name: String,
        /// User account email
        #[arg(long, value_parser = NonEmptyStringValueParser::new())]
        account: String,
        /// User project (omit for none)
        #[arg(long)]
        project: Option<String>,
        /// ADC account email (defaults to the user account)
        #[arg(long, value_parser = NonEmptyStringValueParser::new())]
        adc_account: Option<String>,
        /// ADC quota project (defaults to the user project; pass "" for none)
        #[arg(long)]
        adc_quota_project: Option<String>,
    },
    /// List all profiles with their active parts
    List,
    /// Show what gcloud holds now: the active configuration and the live ADC, each with its
    /// profile and whether its credential is valid
    Status,
    /// Activate a profile: its gcloud configuration, its ADC, or both (default)
    Activate {
        /// Profile name
        name: String,
        /// Only the user configuration (gcloud config)
        #[arg(long, conflicts_with = "adc")]
        user: bool,
        /// Only the Application Default Credentials
        #[arg(long)]
        adc: bool,
    },
    /// Log in for a profile's user credentials, ADC, or both (default) without activating it
    Authenticate {
        /// Profile name
        name: String,
        /// Only the user credentials (gcloud auth login)
        #[arg(long, conflicts_with = "adc")]
        user: bool,
        /// Only the Application Default Credentials
        #[arg(long)]
        adc: bool,
    },
    /// Import existing gcloud configurations
    Import,
    /// Check for and install new releases from GitHub
    SelfUpdate {
        /// Do not open the documentation site after installing
        #[arg(long)]
        no_open_readme: bool,
        /// Only check if an update is available; do not install
        #[arg(long)]
        check_only: bool,
        /// Skip SHA-256 checksum verification (use only if the release predates sidecar support)
        #[arg(long)]
        skip_checksum: bool,
    },
    /// Sync profile metadata (profiles.toml only) via a Git remote
    Sync {
        #[command(subcommand)]
        sub: SyncSub,
    },
    /// Open the documentation site in the browser
    OpenReadme,
    /// Generate shell completion script
    Completion {
        /// Shell to generate completions for: bash, zsh, fish, powershell (default: zsh on macOS)
        shell: Option<String>,
        /// Install the completion script to the default location for the shell (default: true on macOS when no shell is specified)
        #[arg(long)]
        install: bool,
    },
    /// Print the global settings file (gcloud-switch.toml) and its path
    ShowConfig,
}

#[derive(Subcommand)]
enum SyncSub {
    /// Set remote URL and optionally clone (run first before push/pull)
    Init {
        /// Git remote URL (e.g. https://github.com/user/repo.git or git@github.com:user/repo.git)
        remote_url: String,
        /// Branch name (default: main)
        #[arg(long, default_value = "main")]
        branch: String,
    },
    /// Push current profiles to the remote
    Push,
    /// Pull and merge profiles from the remote (newer wins per profile)
    Pull,
}

/// User-level parameters in ~/.config/gcloud-switch/gcloud-switch.toml. Profile data stays in profiles.toml.
/// Created on first run with defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GlobalSettings {
    /// When to check for updates: "never", "always", "daily". Default "always".
    #[serde(default = "default_self_update_frequency")]
    self_update_frequency: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_update_check: Option<String>,
    /// Git remote URL for syncing profiles (e.g. https://github.com/user/repo.git)
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_url: Option<String>,
    /// Branch name for sync (default: main)
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    /// List of filenames to sync (default: ["profiles.toml"])
    #[serde(default = "default_sync_files")]
    sync_files: Vec<String>,
}

fn default_sync_files() -> Vec<String> {
    vec!["profiles.toml".to_string()]
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            self_update_frequency: default_self_update_frequency(),
            last_update_check: None,
            remote_url: None,
            branch: None,
            sync_files: default_sync_files(),
        }
    }
}

fn default_self_update_frequency() -> String {
    "always".to_string()
}

fn global_settings_path() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(|home| {
        PathBuf::from(home).join(".config").join("gcloud-switch").join("gcloud-switch.toml")
    })
}

/// Load global settings. If the file does not exist, create ~/.config/gcloud-switch/gcloud-switch.toml
/// with default values. A file that exists but cannot be read or parsed is an error: resetting it
/// to defaults would be written back over the user's file by the next save.
fn load_global_settings() -> Result<GlobalSettings> {
    let path = global_settings_path().context("Could not determine config directory")?;
    if path.exists() {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        return toml::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()));
    }
    // First run: create directory and write defaults
    let defaults = GlobalSettings::default();
    save_global_settings(&defaults)?;
    Ok(defaults)
}

fn save_global_settings(settings: &GlobalSettings) -> Result<()> {
    let path = global_settings_path().context("Could not determine config directory")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let toml = toml::to_string_pretty(settings).context("Serialize global settings")?;
    std::fs::write(&path, toml)?;
    Ok(())
}

fn check_update_available(client: &reqwest::blocking::Client) -> Result<Option<(String, String)>> {
    let url = format!("{}/{}/releases/latest", API_URL, REPO);
    let response = client.get(&url).send()?;
    if !response.status().is_success() {
        return Ok(None);
    }
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
        html_url: String,
    }
    let release: Release = response.json()?;
    let latest_version = release.tag_name.trim_start_matches('v').to_string();
    let current = env!("CARGO_PKG_VERSION");
    if compare_versions(current, &latest_version) < 0 {
        Ok(Some((latest_version, release.html_url)))
    } else {
        Ok(None)
    }
}

fn maybe_check_for_updates(settings: &mut GlobalSettings) -> Result<()> {
    let freq = settings.self_update_frequency.as_str();
    if freq == "never" {
        return Ok(());
    }
    if freq == "daily" {
        if let Some(ref last) = settings.last_update_check {
            let last_ts: u64 = last.parse().unwrap_or(0);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if now.saturating_sub(last_ts) < 86400 {
                return Ok(());
            }
        }
    }
    let client = reqwest::blocking::Client::builder()
        .user_agent("gcloud-switch-update-checker")
        .build()?;
    let update = check_update_available(&client)?;
    if freq == "daily" {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        settings.last_update_check = Some(now.to_string());
        let _ = save_global_settings(settings);
    }
    if let Some((version, url)) = update {
        println!(
            "⚠️  Update available: {} (current: {}). Run `gcloud-switch self-update` to install. {}",
            version,
            env!("CARGO_PKG_VERSION"),
            url
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    // Parse into matches first: the subcommand NAME is what --html-help needs, and clap
    // only hands it out at this level.
    let matches = Cli::command().get_matches();
    let subcommand = matches.subcommand_name().map(str::to_string);
    let cli = Cli::from_arg_matches(&matches)?;
    if cli.html_help {
        return open_html_help(subcommand.as_deref());
    }

    // Load/create global settings on first run (creates ~/.config/gcloud-switch/gcloud-switch.toml with defaults)
    let mut global_settings = load_global_settings()?;
    // Optional: check for updates per global settings
    if !matches!(cli.command, Some(Commands::SelfUpdate { .. }) | Some(Commands::OpenReadme) | Some(Commands::Completion { .. }) | Some(Commands::ShowConfig) | Some(Commands::Status)) {
        let _ = maybe_check_for_updates(&mut global_settings);
    }

    match cli.command {
        Some(Commands::Add {
            name,
            account,
            project,
            adc_account,
            adc_quota_project,
        }) => {
            let store = Store::new()?;
            let data = store.load_profiles()?;
            // Empty means none for both projects.
            let project = project.map(|p| p.trim().to_string()).unwrap_or_default();
            let adc_quota_project = adc_quota_project
                .map(|p| p.trim().to_string())
                .unwrap_or_else(|| project.clone());
            let profile = Profile {
                adc_account: adc_account.unwrap_or_else(|| account.clone()),
                user_account: account,
                user_project: project,
                adc_quota_project,
                updated_at: None,
            };
            // Create gcloud configuration first so the profile won't be orphaned
            if matches!(data.sync_mode, SyncMode::Strict | SyncMode::Add) {
                gcloud::write_configuration(&name, &profile.user_account, &profile.user_project)?;
            }
            store.add_profile(&name, profile)?;
            println!("Profile '{}' added.", name);
        }
        Some(Commands::List) => {
            let store = Store::new()?;
            let data = store.load_profiles()?;
            if data.profiles.is_empty() {
                println!("No profiles configured. Use 'gcloud-switch add' or press 'n' in the TUI.");
            } else {
                print!("{}", list_table(&store, &data)?);
            }
        }
        Some(Commands::Status) => {
            let store = Store::new()?;
            let data = store.load_profiles()?;
            let state = gcloud::live_state(&store, &data.profiles, data.active_profile.as_deref())?;
            let user_valid = gcloud::check_live_user_auth(&state);
            let adc_valid = gcloud::check_live_adc_auth(&state)?;
            print!("{}", status_report(&state, user_valid, adc_valid));
        }
        Some(Commands::Activate { name, user, adc }) => {
            let store = Store::new()?;
            let mut data = store.load_profiles()?;
            let profile = data
                .profiles
                .get(&name)
                .ok_or_else(|| anyhow::anyhow!("Profile '{}' not found", name))?
                .clone();
            let parts = parts_from_flags(user, adc);

            // Log in first for the parts whose credentials are invalid (matches the TUI).
            let needing = gcloud::parts_needing_auth(&store, &name, &profile, parts);
            if needing.user {
                println!(
                    "User credentials of {} are missing or expired. Logging in...",
                    profile.user_account
                );
            }
            if needing.adc {
                println!(
                    "ADC of profile '{}' is missing or expired. Logging in as {}...",
                    name, profile.adc_account
                );
            }
            if needing.any() {
                gcloud::authenticate(&store, &name, &profile, needing)?;
            }

            gcloud::activate(&store, &name, &profile, parts)?;
            if parts.user {
                data.active_profile = Some(name.clone());
                store.save_profiles(&data)?;
            }
            println!("{}", gcloud::activation_message(&name, &profile, parts));
        }
        Some(Commands::Authenticate { name, user, adc }) => {
            let store = Store::new()?;
            let data = store.load_profiles()?;
            let parts = parts_from_flags(user, adc);
            gcloud::authenticate_only(
                &store,
                &data.profiles,
                data.active_profile.as_deref(),
                &name,
                parts,
            )?;
            println!("Authenticated {}.", parts.describe(&name));
        }
        Some(Commands::Import) => {
            let store = Store::new()?;
            let count = import_profiles(&store)?;
            if count == 0 {
                println!("No new gcloud configurations found to import.");
            }
        }
        Some(Commands::SelfUpdate {
            no_open_readme,
            check_only,
            skip_checksum,
        }) => {
            run_self_update(!no_open_readme, check_only, skip_checksum)?;
        }
        Some(Commands::OpenReadme) => {
            open_url(DOCS_URL)?;
        }
        Some(Commands::Completion { shell, install }) => {
            let using_default = shell.is_none();
            #[cfg(target_os = "macos")]
            let shell = shell.unwrap_or_else(|| "zsh".to_string());
            #[cfg(not(target_os = "macos"))]
            let shell = shell.ok_or_else(|| anyhow::anyhow!("Shell argument is required"))?;
            let install = install || (using_default && cfg!(target_os = "macos"));
            run_completion(&shell, install)?;
        }
        Some(Commands::ShowConfig) => {
            // load_global_settings() created the file with defaults if it was missing.
            let path = global_settings_path().context("Could not determine config directory")?;
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            println!("# {}", path.display());
            print!("{}", content);
        }
        Some(Commands::Sync { sub }) => {
            let store = Store::new()?;
            match sub {
                SyncSub::Init { remote_url, branch } => {
                    global_settings.remote_url = Some(remote_url.clone());
                    global_settings.branch = Some(branch.clone());
                    save_global_settings(&global_settings)?;
                    println!("Sync config saved. Run 'gcloud-switch sync push' to push, or 'sync pull' to pull.");
                    sync::ensure_cloned(&store, &remote_url, &branch)?;
                    println!("Remote cloned to {}.", store.sync_repo_path().display());
                }
                SyncSub::Push => {
                    let remote_url = global_settings.remote_url.as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Sync not configured. Run 'gcloud-switch sync init <remote_url>' first."))?;
                    let branch = global_settings.branch.as_deref().unwrap_or("main");
                    sync::sync_push(&store, remote_url, branch, &global_settings.sync_files)?;
                    println!("Pushed profiles to remote.");
                }
                SyncSub::Pull => {
                    let remote_url = global_settings.remote_url.as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Sync not configured. Run 'gcloud-switch sync init <remote_url>' first."))?;
                    let branch = global_settings.branch.as_deref().unwrap_or("main");
                    sync::sync_pull(&store, remote_url, branch, &global_settings.sync_files)?;
                    println!("Pulled and merged profiles from remote.");
                }
            }
        }
        None => {
            run_tui()?;
        }
    }

    Ok(())
}

/// `--user` / `--adc` flags to the parts they select; neither means both.
fn parts_from_flags(user: bool, adc: bool) -> Parts {
    Parts {
        user: user || !adc,
        adc: adc || !user,
    }
}

/// The `list` output: one row per profile, marking the parts that are live in gcloud.
fn list_table(store: &Store, data: &ProfilesFile) -> Result<String> {
    let active_user = gcloud::read_active_config()?.filter(|n| data.profiles.contains_key(n));
    let active_adc =
        gcloud::active_adc_profile(store, data.profiles.keys(), data.active_profile.as_deref())?;
    let dash = |s: &str| if s.is_empty() { "-".to_string() } else { s.to_string() };
    let rows: Vec<Vec<String>> = data
        .profiles
        .iter()
        .map(|(name, p)| {
            let active = Parts {
                user: active_user.as_deref() == Some(name.as_str()),
                adc: active_adc.as_deref() == Some(name.as_str()),
            };
            vec![
                name.clone(),
                active.marker().to_string(),
                dash(&p.user_account),
                dash(&p.user_project),
                dash(&p.adc_account),
                dash(&p.adc_quota_project),
            ]
        })
        .collect();
    Ok(format_table(
        &["NAME", "ACTIVE", "USER ACCOUNT", "PROJECT", "ADC ACCOUNT", "QUOTA PROJECT"],
        &rows,
    ))
}

/// The `status` output: one line per part with what gcloud holds, any drift from the
/// profiles, and whether that credential is valid.
fn status_report(
    state: &gcloud::LiveState,
    user_valid: Option<bool>,
    adc_valid: Option<bool>,
) -> String {
    let validity = |valid: Option<bool>| match valid {
        Some(true) => " \u{00b7} valid",
        Some(false) => " \u{00b7} INVALID",
        None => "",
    };
    let line = |label: &str, value: Option<(String, Option<String>)>, valid: Option<bool>| {
        match value {
            Some((description, note)) => format!(
                "{:<14} {}{}{}\n",
                label,
                description,
                note.map(|n| format!(" ({})", n)).unwrap_or_default(),
                validity(valid)
            ),
            None => format!("{:<14} none\n", label),
        }
    };
    line(
        "configuration",
        state.configuration.as_ref().map(|c| (c.describe(), c.drift_note())),
        user_valid,
    ) + &line(
        "ADC",
        state.adc.as_ref().map(|a| (a.describe(), a.drift_note())),
        adc_valid,
    )
}

/// Left-aligned columns separated by two spaces, one line per row, no trailing spaces.
fn format_table(header: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = header.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }
    let line = |cells: &[&str]| -> String {
        let mut out = String::new();
        for (i, cell) in cells.iter().enumerate() {
            out.push_str(&format!("{:<width$}  ", cell, width = widths[i]));
        }
        out.trim_end().to_string() + "\n"
    };
    let mut out = line(header);
    for row in rows {
        let cells: Vec<&str> = row.iter().map(String::as_str).collect();
        out.push_str(&line(&cells));
    }
    out
}

fn import_profiles(store: &Store) -> Result<usize> {
    let configs = gcloud::importable_configs()?;
    if configs.is_empty() {
        return Ok(0);
    }

    let mut data = store.load_profiles()?;
    let mut count = 0;

    for (name, account, project) in &configs {
        if data.profiles.contains_key(name) {
            println!("Skipping '{}' (already exists).", name);
            continue;
        }

        let mut profile = Profile {
            user_account: account.clone(),
            user_project: project.clone(),
            adc_account: account.clone(),
            adc_quota_project: project.clone(),
            updated_at: None,
        };
        profile.touch();
        data.profiles.insert(name.clone(), profile);
        println!("Imported '{}'.", name);
        count += 1;
    }

    // Set active profile from gcloud's active configuration
    if count > 0 {
        if let Ok(Some(active)) = gcloud::read_active_config() {
            if data.profiles.contains_key(&active) {
                data.active_profile = Some(active.clone());
                println!("Active profile set to '{}'.", active);
            }
        }
        store.save_profiles(&data)?;
    }

    Ok(count)
}

fn sync_on_startup(store: &Store) -> Result<()> {
    let mut data = store.load_profiles()?;

    // First run: import if no profiles exist
    if data.profiles.is_empty() {
        import_profiles(store)?;
        return Ok(());
    }

    let mut changed = false;

    match data.sync_mode {
        SyncMode::Off => {}
        SyncMode::Add | SyncMode::Strict => {
            // Every configuration counts as existing (strict mode deletes profiles whose
            // configuration is gone), but only those with an account become profiles.
            let config_names: std::collections::HashSet<String> =
                gcloud::discover_existing_configs()?
                    .into_iter()
                    .map(|(n, _, _)| n)
                    .collect();

            // Add new gcloud configs as profiles
            for (name, account, project) in &gcloud::importable_configs()? {
                if !data.profiles.contains_key(name) {
                    let mut profile = Profile {
                        user_account: account.clone(),
                        user_project: project.clone(),
                        adc_account: account.clone(),
                        adc_quota_project: project.clone(),
                        updated_at: None,
                    };
                    profile.touch();
                    data.profiles.insert(name.clone(), profile);
                    changed = true;
                }
            }

            // In strict mode, delete profiles whose gcloud configs no longer exist
            if data.sync_mode == SyncMode::Strict {
                let to_delete: Vec<String> = data
                    .profiles
                    .keys()
                    .filter(|name| !config_names.contains(*name))
                    .cloned()
                    .collect();
                for name in &to_delete {
                    data.profiles.remove(name);
                    if data.active_profile.as_deref() == Some(name) {
                        data.active_profile = None;
                    }
                    // Remove ADC file if it exists
                    let adc_path = store.adc_path(name);
                    if adc_path.exists() {
                        let _ = std::fs::remove_file(adc_path);
                    }
                    changed = true;
                }
            }
        }
    }

    // Always sync active config from gcloud
    if let Ok(Some(active)) = gcloud::read_active_config() {
        if data.profiles.contains_key(&active) && data.active_profile.as_deref() != Some(&active) {
            data.active_profile = Some(active);
            changed = true;
        }
    }

    if changed {
        store.save_profiles(&data)?;
    }

    Ok(())
}

fn run_tui() -> Result<()> {
    let store = Store::new()?;
    sync_on_startup(&store)?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new()?;

    let loop_result: Result<()> = (|| {
        loop {
            app.check_auth_results();
            app.check_project_results();
            terminal.draw(|frame| ui::draw(frame, &mut app))?;

            if app.handle_event()? {
                break;
            }

            // Handle pending actions that need TUI suspended (interactive gcloud commands)
            if !matches!(app.pending_action, PendingAction::None) {
                let pending = std::mem::replace(&mut app.pending_action, PendingAction::None);
                let is_activate = matches!(pending, PendingAction::ReauthAndActivate { .. });

                // Suspend TUI: leave alternate screen and restore normal terminal mode
                disable_raw_mode()?;
                execute!(
                    io::stdout(),
                    LeaveAlternateScreen,
                    DisableMouseCapture,
                    crossterm::cursor::Show
                )?;
                {
                    use std::io::Write;
                    io::stdout().flush()?;
                }

                // Run the interactive gcloud commands (and, for Enter, the activation)
                match app.execute_pending(pending) {
                    Ok(()) => {
                        // The status message is printed once the terminal is restored below.
                        if is_activate && app.quit_after_activate {
                            return Ok(());
                        }
                    }
                    Err(e) => {
                        // Show the failure in the TUI instead of quitting on it.
                        app.quit_after_activate = false;
                        app.status_message = Some(format!("Failed: {:#}", e));
                    }
                }

                // Resume TUI
                enable_raw_mode()?;
                execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
                // Force ratatui to do a full redraw since the screen was cleared
                terminal.clear()?;
            }
        }
        Ok(())
    })();

    // Always restore terminal, even if the loop returned an error
    let _ = disable_raw_mode();
    let _ = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        crossterm::style::ResetColor,
        crossterm::cursor::MoveToColumn(0)
    );
    let _ = terminal.show_cursor();
    use std::io::Write;
    let _ = io::stdout().flush();

    // Print final status message if any
    if let Some(msg) = &app.status_message {
        print!("\r\n{}\r\n", msg);
        let _ = io::stdout().flush();
    }

    loop_result
}

const REPO: &str = "tjirsch/gcloud-switch";
const API_URL: &str = "https://api.github.com/repos";
/// The documentation site: README.md and docs/*.md rendered from this repository's
/// Markdown on every release tag (`.github/workflows/pages.yml`).
const DOCS_URL: &str = "https://tjirsch.github.io/gcloud-switch/";

#[cfg_attr(windows, allow(unused_variables))]
fn run_self_update(open_docs: bool, check_only: bool, skip_checksum: bool) -> Result<()> {
    let current_version = env!("CARGO_PKG_VERSION");
    println!("Current version: {}", current_version);

    let client = reqwest::blocking::Client::builder()
        .user_agent("gcloud-switch-update-checker")
        .build()?;

    let url = format!("{}/{}/releases/latest", API_URL, REPO);
    let response = client.get(&url).send()?;

    if !response.status().is_success() {
        anyhow::bail!("Failed to fetch release info: {}", response.status());
    }

    #[derive(Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
    }

    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
        html_url: String,
        #[serde(default)]
        assets: Vec<Asset>,
    }

    let release: Release = response.json()?;
    let latest_version = release.tag_name.trim_start_matches('v');
    println!("Latest version: {}", latest_version);

    if compare_versions(current_version, latest_version) < 0 {
        println!("\n⚠️  A new version is available!");
        println!("   Current: {}", current_version);
        println!("   Latest:  {}", latest_version);
        println!("   Release: {}", release.html_url);
        if check_only {
            println!("\nRun `gcloud-switch self-update` to install.");
            return Ok(());
        }
        println!("\n📥 Installing update...");

        let installer_url = format!(
            "https://github.com/{}/releases/latest/download/gcloud-switch-installer.sh",
            REPO
        );

        // Download installer as bytes for checksum verification
        let installer_bytes = client.get(&installer_url).send()?.bytes()?;

        // Checksum verification
        let checksum_asset = release.assets.iter()
            .find(|a| a.name == "gcloud-switch-installer.sh.sha256");
        match checksum_asset {
            Some(asset) => {
                let expected_raw = client.get(&asset.browser_download_url)
                    .send()?.text()?;
                let expected = expected_raw.split_whitespace().next().unwrap_or("").to_lowercase();
                use sha2::{Digest, Sha256};
                let actual = hex::encode(Sha256::digest(&installer_bytes));
                if actual != expected {
                    anyhow::bail!(
                        "Checksum mismatch — installer may have been tampered with.\n\
                         Expected: {}\n\
                         Got:      {}\n\
                         Aborting. Download the release manually from {}",
                        expected, actual, release.html_url
                    );
                }
                println!("✅ Checksum verified");
            }
            None if skip_checksum => {
                eprintln!(
                    "⚠️  No checksum file found in this release. \
                     Proceeding without verification (--skip-checksum)."
                );
            }
            None => {
                anyhow::bail!(
                    "No checksum file (gcloud-switch-installer.sh.sha256) found in this release.\n\
                     Cannot verify installer integrity. Aborting.\n\
                     If you are confident in the download, re-run with --skip-checksum."
                );
            }
        }

        let temp_file = std::env::temp_dir()
            .join(format!("gcloud-switch-installer-{}.sh", std::process::id()));
        std::fs::write(&temp_file, &installer_bytes)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temp_file, std::fs::Permissions::from_mode(0o755))?;

            let status = std::process::Command::new("sh").arg(&temp_file).status()?;
            let _ = std::fs::remove_file(&temp_file);

            if status.success() {
                println!("✅ Update installed successfully!");
                println!("   Please restart your terminal or run: source ~/.profile");
                println!("   Documentation: {}", DOCS_URL);
                if open_docs {
                    open_url(DOCS_URL)?;
                }
            } else {
                anyhow::bail!("Failed to run installer script");
            }
        }

        #[cfg(windows)]
        {
            anyhow::bail!(
                "Automatic installation on Windows is not yet supported. Please download and run the installer manually."
            );
        }
    } else {
        println!("✅ You are running the latest version!");
    }

    Ok(())
}

/// Open a URL in the default browser (never an editor).
fn open_url(url: &str) -> Result<()> {
    println!("Opening {}", url);
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("open").arg(url).status();
    #[cfg(target_os = "linux")]
    let status = std::process::Command::new("xdg-open").arg(url).status();
    #[cfg(target_os = "windows")]
    let status = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .status();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let status: std::io::Result<std::process::ExitStatus> =
        Err(std::io::Error::other("no browser opener on this platform"));
    match status {
        Ok(st) if st.success() => Ok(()),
        Ok(st) => anyhow::bail!("could not open {}: the opener exited with {}", url, st),
        Err(e) => anyhow::bail!("could not open {}: {}", url, e),
    }
}

/// `--html-help`: the documentation site, at the invoked command's section when it has one.
fn open_html_help(subcommand: Option<&str>) -> Result<()> {
    // Commands with a `### … (`<cmd>`)` section in README.md; scripts/build-site.py gives
    // such a heading `id="cmd-<cmd>"`.
    const DOCUMENTED: &[&str] = &[
        "add",
        "list",
        "status",
        "activate",
        "authenticate",
        "import",
        "sync",
        "self-update",
        "open-readme",
        "completion",
        "show-config",
    ];
    match subcommand {
        Some(cmd) if DOCUMENTED.contains(&cmd) => open_url(&format!("{}#cmd-{}", DOCS_URL, cmd)),
        Some(cmd) => {
            println!(
                "No section for `{}` on the documentation site yet; opening the command list.",
                cmd
            );
            open_url(&format!("{}#cli", DOCS_URL))
        }
        None => open_url(DOCS_URL),
    }
}

fn run_completion(shell_str: &str, install: bool) -> Result<()> {
    use clap_complete::{generate, Shell};
    use std::str::FromStr;

    let shell = Shell::from_str(shell_str).map_err(|_| {
        anyhow::anyhow!(
            "Unknown shell '{}'. Supported shells: bash, zsh, fish, powershell",
            shell_str
        )
    })?;

    let mut cmd = Cli::command();
    let bin_name = "gcloud-switch";

    if install {
        let (path, post_install_msg) = completion_install_path(shell)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(&path)?;
        generate(shell, &mut cmd, bin_name, &mut file);
        println!("Completion script installed to: {}", path.display());
        if let Some(msg) = post_install_msg {
            println!("{}", msg);
        }
    } else {
        generate(shell, &mut cmd, bin_name, &mut std::io::stdout());
    }

    Ok(())
}

fn completion_install_path(shell: clap_complete::Shell) -> Result<(std::path::PathBuf, Option<String>)> {
    use clap_complete::Shell;
    let home = std::env::var("HOME").unwrap_or_else(|_| "~".to_string());
    let (path, msg): (std::path::PathBuf, Option<String>) = match shell {
        Shell::Bash => (
            std::path::PathBuf::from(format!(
                "{}/.local/share/bash-completion/completions/gcloud-switch",
                home
            )),
            Some("Ensure bash-completion is installed and sourced in your ~/.bashrc".to_string()),
        ),
        Shell::Zsh => (
            std::path::PathBuf::from(format!("{}/.zsh/completions/_gcloud-switch", home)),
            Some(
                "Ensure ~/.zsh/completions is in your fpath — add to ~/.zshrc:\n\
                   fpath=(~/.zsh/completions $fpath)\n\
                   autoload -Uz compinit && compinit"
                    .to_string(),
            ),
        ),
        Shell::Fish => (
            std::path::PathBuf::from(format!(
                "{}/.config/fish/completions/gcloud-switch.fish",
                home
            )),
            None,
        ),
        Shell::PowerShell => {
            let userprofile = std::env::var("USERPROFILE").unwrap_or_else(|_| home.clone());
            (
                std::path::PathBuf::from(format!(
                    r"{}\Documents\PowerShell\Completions\gcloud-switch.ps1",
                    userprofile
                )),
                Some(
                    "Add to your $PROFILE:\n\
                       . \"$env:USERPROFILE\\Documents\\PowerShell\\Completions\\gcloud-switch.ps1\""
                        .to_string(),
                ),
            )
        }
        _ => anyhow::bail!("Unsupported shell: {:?}", shell),
    };
    Ok((path, msg))
}

fn compare_versions(v1: &str, v2: &str) -> i32 {
    let parse_version = |v: &str| -> Vec<u32> { v.split('.').map(|s| s.parse::<u32>().unwrap_or(0)).collect() };
    let v1_parts = parse_version(v1);
    let v2_parts = parse_version(v2);
    let max_len = v1_parts.len().max(v2_parts.len());
    for i in 0..max_len {
        let a = v1_parts.get(i).copied().unwrap_or(0);
        let b = v2_parts.get(i).copied().unwrap_or(0);
        if a < b {
            return -1;
        }
        if a > b {
            return 1;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    #[test]
    fn status_report_shows_both_parts_with_drift_and_validity() {
        use crate::gcloud::{LiveAdc, LiveConfiguration, LiveState};
        let state = LiveState {
            configuration: Some(LiveConfiguration {
                name: "mmt01".into(),
                account: "a@x.com".into(),
                project: "p".into(),
                profile: Some("mmt01".into()),
                differs: true,
            }),
            adc: Some(LiveAdc {
                account: "a@x.com".into(),
                quota_project: String::new(),
                profile: Some("mmt01".into()),
                differs: false,
            }),
        };
        assert_eq!(
            super::status_report(&state, Some(true), Some(false)),
            "configuration  mmt01 \u{00b7} a@x.com \u{00b7} project p (differs from profile 'mmt01') \u{00b7} valid\n\
             ADC            mmt01 \u{00b7} a@x.com \u{00b7} no quota project \u{00b7} INVALID\n"
        );
        assert_eq!(
            super::status_report(&LiveState::default(), None, None),
            "configuration  none\nADC            none\n"
        );
    }

    use super::*;

    #[test]
    fn format_table_aligns_columns_and_trims_trailing_spaces() {
        let rows = vec![
            vec!["a".to_string(), "both".to_string(), "x".to_string()],
            vec!["longer".to_string(), "-".to_string(), String::new()],
        ];
        let out = format_table(&["NAME", "ACTIVE", "LAST"], &rows);
        assert_eq!(out, "NAME    ACTIVE  LAST\na       both    x\nlonger  -\n");
    }

    #[test]
    fn parts_from_flags_defaults_to_both() {
        assert_eq!(parts_from_flags(false, false), Parts::BOTH);
        assert_eq!(parts_from_flags(true, false), Parts::USER);
        assert_eq!(parts_from_flags(false, true), Parts::ADC);
    }
}
