mod app;
mod gcloud;
mod profile;
mod store;
mod sync;
mod ui;

use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::app::{App, PendingAction};
use crate::profile::{Profile, SyncMode};
use crate::store::Store;

#[derive(Parser)]
#[command(name = "gcloud-switch", version, about = "TUI Google Cloud profile switcher")]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Add a new profile
    Add {
        /// Profile name
        name: String,
        /// User account email
        #[arg(long)]
        account: String,
        /// User project
        #[arg(long)]
        project: String,
        /// ADC account email (defaults to user account)
        #[arg(long)]
        adc_account: Option<String>,
        /// ADC quota project (defaults to user project)
        #[arg(long)]
        adc_quota_project: Option<String>,
    },
    /// List all profiles
    List,
    /// Switch to a profile
    Switch {
        /// Profile name
        name: String,
    },
    /// Import existing gcloud configurations
    Import,
    /// Check for and install new releases from GitHub
    SelfUpdate {
        /// Do not download README.md after installing
        #[arg(long)]
        no_download_readme: bool,
        /// Do not open README.md after downloading (only applies if download runs)
        #[arg(long)]
        no_open_readme: bool,
        /// Only check if an update is available; do not install or download README
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
    /// Download and open the latest README from the repository
    OpenReadme,
    /// Generate shell completion script
    Completion {
        /// Shell to generate completions for: bash, zsh, fish, powershell (default: zsh on macOS)
        shell: Option<String>,
        /// Install the completion script to the default location for the shell (default: true on macOS when no shell is specified)
        #[arg(long)]
        install: bool,
    },
    /// Set (or clear) the editor in global settings
    SetEditor {
        /// Editor command to use (e.g. "code", "zed", "vim"). Omit to show current value.
        editor: Option<String>,
        /// Remove the editor setting (fall back to $EDITOR / OS default)
        #[arg(long)]
        clear: bool,
    },
    /// Print the global config file (gcloud-switch.toml)
    ShowConfig,
    /// Open the global config file in an editor
    EditConfig,
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
    /// Editor command for opening files (e.g. "code", "zed", "vim").
    /// Falls back to $EDITOR env var, then the OS default app.
    #[serde(skip_serializing_if = "Option::is_none")]
    editor: Option<String>,
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
            editor: None,
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

/// Load global settings. If the file does not exist, create ~/.config/gcloud-switch/gcloud-switch.toml with default values.
fn load_global_settings() -> GlobalSettings {
    let path = match global_settings_path() {
        Some(p) => p,
        None => return GlobalSettings::default(),
    };
    if path.exists() {
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return GlobalSettings::default(),
        };
        return toml::from_str(&content).unwrap_or_default();
    }
    // First run: create directory and write defaults
    let defaults = GlobalSettings::default();
    let _ = save_global_settings(&defaults);
    defaults
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
    let cli = Cli::parse();

    // Load/create global settings on first run (creates ~/.config/gcloud-switch/gcloud-switch.toml with defaults)
    let mut global_settings = load_global_settings();
    // Optional: check for updates per global settings
    if !matches!(cli.command, Some(Commands::SelfUpdate { .. }) | Some(Commands::OpenReadme) | Some(Commands::Completion { .. }) | Some(Commands::SetEditor { .. }) | Some(Commands::ShowConfig) | Some(Commands::EditConfig)) {
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
            let profile = Profile {
                user_account: account.clone(),
                user_project: project.clone(),
                adc_account: adc_account.unwrap_or_else(|| account.clone()),
                adc_quota_project: adc_quota_project.unwrap_or_else(|| project.clone()),
                updated_at: None,
            };
            // Create gcloud configuration first so the profile won't be orphaned
            if matches!(data.sync_mode, SyncMode::Strict | SyncMode::Add) {
                gcloud::create_configuration(&name, &profile.user_account, &profile.user_project)?;
            }
            store.add_profile(&name, profile.clone())?;
            println!("Profile '{}' added.", name);
        }
        Some(Commands::List) => {
            let store = Store::new()?;
            let data = store.load_profiles()?;
            if data.profiles.is_empty() {
                println!("No profiles configured. Use 'gcloud-switch add' or press 'a' in the TUI.");
            } else {
                for (name, profile) in &data.profiles {
                    let active = if data.active_profile.as_deref() == Some(name.as_str()) {
                        " (active)"
                    } else {
                        ""
                    };
                    println!(
                        "{}{}: user={}@{} adc={}@{}",
                        name,
                        active,
                        profile.user_account,
                        profile.user_project,
                        profile.adc_account,
                        profile.adc_quota_project,
                    );
                }
            }
        }
        Some(Commands::Switch { name }) => {
            let store = Store::new()?;
            let mut data = store.load_profiles()?;
            let profile = data
                .profiles
                .get(&name)
                .ok_or_else(|| anyhow::anyhow!("Profile '{}' not found", name))?
                .clone();

            // Check auth before activation (matches TUI behavior)
            if !gcloud::check_account_auth(&profile.user_account) {
                println!(
                    "Credentials expired for '{}'. Re-authenticating...",
                    profile.user_account
                );
                gcloud::reauth_user(&profile.user_account)?;
            }

            gcloud::activate_both(&store, &name, &profile.user_account, &profile.user_project)?;
            data.active_profile = Some(name.clone());
            store.save_profiles(&data)?;
            println!("Switched to profile '{}'.", name);
        }
        Some(Commands::Import) => {
            let store = Store::new()?;
            let count = import_profiles(&store)?;
            if count == 0 {
                println!("No new gcloud configurations found to import.");
            }
        }
        Some(Commands::SelfUpdate {
            no_download_readme,
            no_open_readme,
            check_only,
            skip_checksum,
        }) => {
            run_self_update(!no_download_readme, !no_open_readme, check_only, skip_checksum, global_settings.editor.as_deref())?;
        }
        Some(Commands::OpenReadme) => {
            run_open_readme(global_settings.editor.as_deref())?;
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
        Some(Commands::SetEditor { editor, clear }) => {
            if clear {
                global_settings.editor = None;
                save_global_settings(&global_settings)?;
                println!("✅ editor cleared (will fall back to $EDITOR / OS default).");
            } else if let Some(e) = editor {
                global_settings.editor = Some(e.clone());
                save_global_settings(&global_settings)?;
                println!("✅ editor set to \"{}\".", e);
            } else {
                match &global_settings.editor {
                    Some(e) => println!("editor = \"{}\"", e),
                    None => println!("editor is not set (using $EDITOR / OS default)."),
                }
            }
        }
        Some(Commands::ShowConfig) => {
            let path = global_settings_path().context("Could not determine config directory")?;
            if path.exists() {
                let content = std::fs::read_to_string(&path)
                    .with_context(|| format!("Failed to read {}", path.display()))?;
                print!("{}", content);
            } else {
                println!("Config file does not exist yet: {}", path.display());
            }
        }
        Some(Commands::EditConfig) => {
            let path = global_settings_path().context("Could not determine config directory")?;
            if !path.exists() {
                // Ensure the file exists before opening
                save_global_settings(&global_settings)?;
            }
            open_file(&path, global_settings.editor.as_deref())?;
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

fn import_profiles(store: &Store) -> Result<usize> {
    let configs = gcloud::discover_existing_configs()?;
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
            let configs = gcloud::discover_existing_configs()?;
            let config_names: std::collections::HashSet<String> =
                configs.iter().map(|(n, _, _)| n.clone()).collect();

            // Add new gcloud configs as profiles
            for (name, account, project) in &configs {
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
                let is_activate = matches!(app.pending_action, PendingAction::ReauthAndActivate);
                app.pending_action = PendingAction::None;

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

                // Run interactive gcloud commands
                let reauth_result = app.execute_reauth();

                // If reauth succeeded and this was an activate flow, do the activation
                if is_activate && reauth_result.is_ok() {
                    let _ = app.do_activate();
                    if app.quit_after_activate {
                        if let Some(msg) = &app.status_message {
                            use std::io::Write;
                            print!("\r\n{}\r\n", msg);
                            io::stdout().flush()?;
                        }
                        return Ok(());
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

const REPO: &str = "tjirsch/rs-gcloud-switch";
const API_URL: &str = "https://api.github.com/repos";

fn run_self_update(download_readme: bool, open_readme: bool, check_only: bool, skip_checksum: bool, editor: Option<&str>) -> Result<()> {
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
                let expected = expected_raw.trim().split_whitespace().next().unwrap_or("").to_lowercase();
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
                if download_readme {
                    match download_and_open_readme(&client, REPO, latest_version, open_readme, editor) {
                        Ok(Some(path)) => println!("README: {}", path.display()),
                        Ok(None) => {}
                        Err(e) => eprintln!("⚠️  Warning: Could not download README: {}", e),
                    }
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

fn download_and_open_readme(
    client: &reqwest::blocking::Client,
    repo: &str,
    version: &str,
    open_after_download: bool,
    editor: Option<&str>,
) -> Result<Option<PathBuf>> {
    let download_dir = get_download_dir()?;
    let readme_path = download_dir.join(format!("gcloud-switch-{}-README.md", version));
    let readme_url = format!("https://raw.githubusercontent.com/{}/main/README.md", repo);
    println!("\n📄 Downloading README...");
    let readme_content = client.get(&readme_url).send()?.text()?;
    std::fs::write(&readme_path, readme_content)?;
    if open_after_download {
        open_file(&readme_path, editor)?;
    }
    Ok(Some(readme_path))
}

fn get_download_dir() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME")?;
        Ok(PathBuf::from(home).join("Downloads"))
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(dir) = std::env::var("XDG_DOWNLOAD_DIR") {
            Ok(PathBuf::from(dir))
        } else {
            let home = std::env::var("HOME")?;
            Ok(PathBuf::from(home).join("Downloads"))
        }
    }

    #[cfg(target_os = "windows")]
    {
        let user_profile = std::env::var("USERPROFILE")?;
        Ok(PathBuf::from(user_profile).join("Downloads"))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        anyhow::bail!("Unsupported platform for download directory");
    }
}

fn open_file(path: &Path, editor: Option<&str>) -> Result<()> {
    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("File path {:?} contains non-UTF-8 characters", path))?;

    let editor_env = std::env::var("EDITOR").ok();
    let editor = editor.or_else(|| editor_env.as_deref());

    if let Some(editor) = editor {
        println!("   Opening '{}' with '{}'...", path_str, editor);
        let result = std::process::Command::new(editor).arg(path).status();
        match result {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                #[cfg(target_os = "macos")]
                {
                    let open_result = std::process::Command::new("open")
                        .args(["-a", editor, path_str])
                        .status();
                    if open_result.map(|s| s.success()).unwrap_or(false) {
                        return Ok(());
                    }
                }
                anyhow::bail!(
                    "Editor '{}' not found — is it installed and on your PATH?\n\
                     Hint: set editor to the full path in ~/.config/gcloud-switch/gcloud-switch.toml\n\
                     e.g.  editor = \"/usr/local/bin/zed\"",
                    editor
                );
            }
            Err(e) => anyhow::bail!("Failed to launch editor '{}': {}", editor, e),
        }
    }

    // No editor configured — use OS default
    #[cfg(target_os = "macos")]
    {
        println!("   Opening '{}' with system default app...", path_str);
        std::process::Command::new("open").arg(path_str).status()?;
    }

    #[cfg(target_os = "linux")]
    {
        println!("   Opening '{}' with xdg-open...", path_str);
        if std::process::Command::new("xdg-open")
            .arg(path_str)
            .status()
            .is_err()
        {
            anyhow::bail!(
                "Could not open '{}': xdg-open failed and neither editor nor $EDITOR is set",
                path_str
            );
        }
    }

    #[cfg(target_os = "windows")]
    {
        println!("   Opening '{}' with system default app...", path_str);
        std::process::Command::new("cmd")
            .args(["/C", "start", "", path_str])
            .status()?;
    }

    Ok(())
}

fn run_open_readme(editor: Option<&str>) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("gcloud-switch-open-readme")
        .build()?;
    println!("📄 Downloading README...");
    match download_and_open_readme(&client, REPO, "latest", true, editor)? {
        Some(path) => println!("README saved to: {}", path.display()),
        None => {}
    }
    Ok(())
}

fn run_completion(shell_str: &str, install: bool) -> Result<()> {
    use clap::CommandFactory;
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
