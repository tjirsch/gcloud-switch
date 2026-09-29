use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;

use crate::gcloud::{self, Parts};
use crate::profile::{Profile, SyncMode};
use crate::store::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Both,
    User,
    Adc,
}

impl Column {
    /// The profile parts this column selection targets.
    pub fn parts(self) -> Parts {
        match self {
            Column::Both => Parts::BOTH,
            Column::User => Parts::USER,
            Column::Adc => Parts::ADC,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    AddProfileName,
    AddProfileUserAccount,
    AddProfileUserProject,
    AddProfileAdcAccount,
    AddProfileAdcQuotaProject,
    ConfirmDelete,
    EditAccount,
    EditProject,
}

/// An interactive gcloud command (browser login) that needs the TUI suspended; the main loop
/// runs it outside the event handler.
pub enum PendingAction {
    None,
    /// Explicit authenticate (`a`) of every part the selected column targets.
    Reauth,
    /// Activation that first needs a login for the parts whose credentials are invalid.
    ReauthAndActivate { needing: Parts },
}

/// Result from a background auth check thread.
struct AuthResult {
    generation: u64,
    profile_index: usize,
    is_user: bool,
    valid: bool,
}

pub struct App {
    pub store: Store,
    pub profile_names: Vec<String>,
    pub profiles: Vec<Profile>,
    /// The profile whose user configuration is gcloud's active configuration.
    pub active_profile: Option<String>,
    /// The profile whose stored ADC credential is the live ADC file.
    pub active_adc: Option<String>,
    pub user_auth_valid: Vec<Option<bool>>,
    pub adc_auth_valid: Vec<Option<bool>>,
    pub selected_row: usize,
    pub selected_col: Column,
    pub should_quit: bool,
    pub status_message: Option<String>,
    pub input_mode: InputMode,
    pub input_buffer: String,
    // Temporary storage for profile being added
    pub new_profile_name: String,
    pub new_profile: Profile,
    // In-place editing state
    pub edit_col: Column,
    pub edit_account_buffer: String,
    pub edit_project_buffer: String,
    pub edit_cursor_pos: usize,
    pub suggestions: Vec<String>,
    pub suggestion_index: Option<usize>,
    // Pending action that needs TUI suspended
    pub pending_action: PendingAction,
    pub quit_after_activate: bool,
    // Async auth check state
    auth_tx: mpsc::Sender<AuthResult>,
    auth_rx: mpsc::Receiver<AuthResult>,
    auth_generation: u64,
    // Async project list fetch state
    project_tx: mpsc::Sender<Vec<String>>,
    project_rx: mpsc::Receiver<Vec<String>>,
    pub fetched_projects: Vec<String>,
    pub fetching_projects: bool,
    pub sync_mode: SyncMode,
    pub table_state: TableState,
}

impl App {
    pub fn new() -> Result<Self> {
        let store = Store::new()?;
        let data = store.load_profiles()?;

        let profile_names: Vec<String> = data.profiles.keys().cloned().collect();
        let profiles: Vec<Profile> = data.profiles.values().cloned().collect();
        let active_adc = gcloud::active_adc_profile(&store, data.profiles.keys())?;
        let active_profile = data.active_profile;
        let sync_mode = data.sync_mode;

        let selected_row = if let Some(ref active) = active_profile {
            profile_names
                .iter()
                .position(|n| n == active)
                .unwrap_or(0)
        } else {
            0
        };

        let (auth_tx, auth_rx) = mpsc::channel();
        let (project_tx, project_rx) = mpsc::channel();

        let mut app = Self {
            store,
            profile_names,
            profiles,
            active_profile,
            active_adc,
            user_auth_valid: Vec::new(),
            adc_auth_valid: Vec::new(),
            selected_row,
            selected_col: Column::Both,
            should_quit: false,
            status_message: None,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            new_profile_name: String::new(),
            new_profile: Profile {
                user_account: String::new(),
                user_project: String::new(),
                adc_account: String::new(),
                adc_quota_project: String::new(),
                updated_at: None,
            },
            edit_col: Column::User,
            edit_account_buffer: String::new(),
            edit_project_buffer: String::new(),
            edit_cursor_pos: 0,
            suggestions: Vec::new(),
            suggestion_index: None,
            pending_action: PendingAction::None,
            quit_after_activate: false,
            auth_tx,
            auth_rx,
            auth_generation: 0,
            project_tx,
            project_rx,
            fetched_projects: Vec::new(),
            fetching_projects: false,
            sync_mode,
            table_state: TableState::default().with_selected(Some(selected_row)),
        };

        app.start_auth_checks();
        Ok(app)
    }

    /// Spawn background threads that check every credential: user credentials once per
    /// account (they live in gcloud's credentials.db), ADC credentials once per profile (they
    /// are stored per profile).
    fn start_auth_checks(&mut self) {
        self.auth_generation += 1;
        let gen = self.auth_generation;
        self.user_auth_valid = vec![None; self.profiles.len()];
        self.adc_auth_valid = vec![None; self.profiles.len()];

        let mut user_targets: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, profile) in self.profiles.iter().enumerate() {
            if profile.user_account.is_empty() {
                self.user_auth_valid[i] = Some(false);
            } else {
                user_targets
                    .entry(profile.user_account.clone())
                    .or_default()
                    .push(i);
            }
        }
        for (account, targets) in user_targets {
            let tx = self.auth_tx.clone();
            std::thread::spawn(move || {
                let valid = gcloud::check_account_auth(&account);
                for idx in targets {
                    let _ = tx.send(AuthResult {
                        generation: gen,
                        profile_index: idx,
                        is_user: true,
                        valid,
                    });
                }
            });
        }

        for (i, (name, profile)) in self.profile_names.iter().zip(&self.profiles).enumerate() {
            if !self.store.has_adc(name) {
                self.adc_auth_valid[i] = Some(false);
                continue;
            }
            let path = self.store.adc_path(name);
            let account = profile.adc_account.clone();
            let tx = self.auth_tx.clone();
            std::thread::spawn(move || {
                let valid = gcloud::check_adc_auth_at(path, account);
                let _ = tx.send(AuthResult {
                    generation: gen,
                    profile_index: i,
                    is_user: false,
                    valid,
                });
            });
        }
    }

    /// Drain completed auth results from background threads.
    pub fn check_auth_results(&mut self) {
        while let Ok(result) = self.auth_rx.try_recv() {
            if result.generation != self.auth_generation {
                continue;
            }
            if result.profile_index >= self.profiles.len() {
                continue;
            }
            if result.is_user {
                self.user_auth_valid[result.profile_index] = Some(result.valid);
            } else {
                self.adc_auth_valid[result.profile_index] = Some(result.valid);
            }
        }
    }

    /// Drain completed project list results from background thread.
    pub fn check_project_results(&mut self) {
        while let Ok(projects) = self.project_rx.try_recv() {
            self.fetched_projects = projects;
            self.fetching_projects = false;
        }
    }

    /// Spawn a background thread to fetch projects for the given account.
    fn start_project_fetch(&mut self, account: &str) {
        if account.is_empty() {
            self.fetched_projects.clear();
            return;
        }
        self.fetching_projects = true;
        self.fetched_projects.clear();
        let account = account.to_string();
        let tx = self.project_tx.clone();
        std::thread::spawn(move || {
            let projects = gcloud::list_projects_for_account(&account).unwrap_or_default();
            let _ = tx.send(projects);
        });
    }

    pub fn reload(&mut self) -> Result<()> {
        let data = self.store.load_profiles()?;
        self.profile_names = data.profiles.keys().cloned().collect();
        self.profiles = data.profiles.values().cloned().collect();
        self.active_profile = data.active_profile;
        self.active_adc = gcloud::active_adc_profile(&self.store, self.profile_names.iter())?;
        if self.selected_row >= self.profile_names.len() {
            self.selected_row = self.profile_names.len().saturating_sub(1);
        }
        self.table_state.select(Some(self.selected_row));
        self.start_auth_checks();
        Ok(())
    }

    pub fn handle_event(&mut self) -> Result<bool> {
        // Use poll with timeout so the UI can refresh for async auth results
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                match self.input_mode {
                    InputMode::Normal => self.handle_normal_key(key)?,
                    InputMode::ConfirmDelete => self.handle_confirm_delete(key)?,
                    InputMode::EditAccount | InputMode::EditProject => {
                        self.handle_edit_key(key)?
                    }
                    _ => self.handle_input_key(key)?,
                }
            }
        }
        Ok(self.should_quit)
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.should_quit = true;
            }
            KeyCode::Up => {
                if !self.profile_names.is_empty() && self.selected_row > 0 {
                    self.selected_row -= 1;
                    self.table_state.select(Some(self.selected_row));
                }
                self.status_message = None;
            }
            KeyCode::Down => {
                if !self.profile_names.is_empty()
                    && self.selected_row < self.profile_names.len() - 1
                {
                    self.selected_row += 1;
                    self.table_state.select(Some(self.selected_row));
                }
                self.status_message = None;
            }
            KeyCode::Left => {
                self.selected_col = match self.selected_col {
                    Column::Both => Column::Both,
                    Column::User => Column::Both,
                    Column::Adc => Column::User,
                };
                self.status_message = None;
            }
            KeyCode::Right => {
                self.selected_col = match self.selected_col {
                    Column::Both => Column::User,
                    Column::User => Column::Adc,
                    Column::Adc => Column::Adc,
                };
                self.status_message = None;
            }
            KeyCode::Enter => {
                if !self.profile_names.is_empty() {
                    self.quit_after_activate = !key.modifiers.contains(KeyModifiers::ALT);
                    match self.activate_selected() {
                        Ok(()) => {
                            // Quit now only when no login is pending; otherwise after it completes.
                            if self.quit_after_activate
                                && matches!(self.pending_action, PendingAction::None)
                            {
                                self.should_quit = true;
                            }
                        }
                        Err(e) => {
                            self.quit_after_activate = false;
                            self.status_message = Some(format!("Activation failed: {:#}", e));
                        }
                    }
                }
            }
            KeyCode::Char('a') => {
                if !self.profile_names.is_empty() {
                    self.pending_action = PendingAction::Reauth;
                }
            }
            KeyCode::Char('n') => {
                self.input_mode = InputMode::AddProfileName;
                self.input_buffer.clear();
                self.status_message = Some("Enter profile name:".to_string());
            }
            KeyCode::Char('e') => {
                if !self.profile_names.is_empty() {
                    let edit_col = match self.selected_col {
                        Column::Both => Column::User,
                        col => col,
                    };
                    let profile = &self.profiles[self.selected_row];
                    self.edit_col = edit_col;
                    self.edit_account_buffer = match edit_col {
                        Column::User => profile.user_account.clone(),
                        Column::Adc => profile.adc_account.clone(),
                        Column::Both => unreachable!("Both is mapped to User above"),
                    };
                    self.edit_project_buffer = match edit_col {
                        Column::User => profile.user_project.clone(),
                        Column::Adc => profile.adc_quota_project.clone(),
                        Column::Both => unreachable!("Both is mapped to User above"),
                    };
                    self.input_mode = InputMode::EditAccount;
                    self.edit_cursor_pos = self.edit_account_buffer.chars().count();
                    self.suggestions.clear();
                    self.suggestion_index = None;
                    self.status_message = None;
                }
            }
            KeyCode::Char('d') => {
                if !self.profile_names.is_empty() {
                    let name = &self.profile_names[self.selected_row];
                    self.status_message =
                        Some(format!("Delete profile '{}'? (y/n)", name));
                    self.input_mode = InputMode::ConfirmDelete;
                }
            }
            KeyCode::Char('s') => {
                self.sync_mode = match self.sync_mode {
                    SyncMode::Strict => SyncMode::Add,
                    SyncMode::Add => SyncMode::Off,
                    SyncMode::Off => SyncMode::Strict,
                };
                let mut data = self.store.load_profiles()?;
                data.sync_mode = self.sync_mode;
                self.store.save_profiles(&data)?;
                let label = match self.sync_mode {
                    SyncMode::Strict => "strict",
                    SyncMode::Add => "add",
                    SyncMode::Off => "off",
                };
                self.status_message = Some(format!("Sync mode: {}", label));
            }
            KeyCode::Char('i') => {
                let configs = gcloud::importable_configs()?;
                if configs.is_empty() {
                    self.status_message = Some("No gcloud configurations found.".to_string());
                } else {
                    let mut data = self.store.load_profiles()?;
                    let mut count = 0;
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
                            count += 1;
                        }
                    }
                    if count > 0 {
                        if let Ok(Some(active)) = gcloud::read_active_config() {
                            if data.profiles.contains_key(&active) {
                                data.active_profile = Some(active);
                            }
                        }
                        self.store.save_profiles(&data)?;
                        self.reload()?;
                        self.status_message =
                            Some(format!("Imported {} profile(s).", count));
                    } else {
                        self.status_message =
                            Some("No new configurations to import.".to_string());
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_input_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.input_buffer.clear();
                self.status_message = None;
            }
            KeyCode::Enter => {
                let value = self.input_buffer.trim().to_string();
                match self.input_mode {
                    InputMode::AddProfileName => {
                        if value.is_empty() {
                            self.status_message =
                                Some("Profile name is required. Enter profile name:".to_string());
                            return Ok(());
                        }
                        self.new_profile_name = value;
                        self.input_buffer.clear();
                        self.input_mode = InputMode::AddProfileUserAccount;
                        self.status_message = Some("Enter user account (email):".to_string());
                    }
                    InputMode::AddProfileUserAccount => {
                        if value.is_empty() {
                            self.status_message = Some(
                                "User account is required. Enter user account (email):"
                                    .to_string(),
                            );
                            return Ok(());
                        }
                        self.new_profile.user_account = value.clone();
                        self.new_profile.adc_account = value; // default
                        self.input_buffer.clear();
                        self.input_mode = InputMode::AddProfileUserProject;
                        self.status_message =
                            Some("Enter user project (empty = none):".to_string());
                    }
                    InputMode::AddProfileUserProject => {
                        // Empty = no project.
                        self.new_profile.user_project = value;
                        self.input_buffer.clear();
                        self.input_mode = InputMode::AddProfileAdcAccount;
                        self.status_message = Some(format!(
                            "Enter ADC account [{}]:",
                            self.new_profile.adc_account
                        ));
                    }
                    InputMode::AddProfileAdcAccount => {
                        // Empty = accept the default from the user account (shown in brackets),
                        // which is never empty here.
                        if !value.is_empty() {
                            self.new_profile.adc_account = value;
                        }
                        // The quota project defaults to the user project. It is prefilled
                        // rather than bracketed so it can be edited or emptied.
                        self.input_buffer = self.new_profile.user_project.clone();
                        self.input_mode = InputMode::AddProfileAdcQuotaProject;
                        self.status_message =
                            Some("Enter ADC quota project (empty = none):".to_string());
                    }
                    InputMode::AddProfileAdcQuotaProject => {
                        // The buffer as shown is the value; empty = no quota project.
                        self.new_profile.adc_quota_project = value;
                        // Create the gcloud configuration first (if sync requires it)
                        if matches!(self.sync_mode, SyncMode::Strict | SyncMode::Add) {
                            if let Err(e) = gcloud::write_configuration(
                                &self.new_profile_name,
                                &self.new_profile.user_account,
                                &self.new_profile.user_project,
                            ) {
                                self.status_message = Some(format!(
                                    "Failed to create gcloud config: {:#}",
                                    e
                                ));
                                self.input_mode = InputMode::Normal;
                                self.input_buffer.clear();
                                return Ok(());
                            }
                        }
                        // Save the profile
                        self.store
                            .add_profile(&self.new_profile_name, self.new_profile.clone())?;
                        self.status_message = Some(format!(
                            "Profile '{}' added.",
                            self.new_profile_name
                        ));
                        self.reload()?;
                        self.input_mode = InputMode::Normal;
                        self.input_buffer.clear();
                    }
                    _ => {}
                }
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
            }
            KeyCode::Char(c) => {
                if self.input_mode == InputMode::AddProfileName {
                    if c.is_ascii_alphanumeric() || c == '-' {
                        self.input_buffer.push(c);
                    }
                } else {
                    self.input_buffer.push(c);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_confirm_delete(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let name = self.profile_names[self.selected_row].clone();
                self.store.delete_profile(&name)?;
                if self.sync_mode == SyncMode::Strict {
                    let _ = gcloud::delete_configuration(&name);
                }
                self.status_message = Some(format!("Deleted profile '{}'.", name));
                self.reload()?;
                self.input_mode = InputMode::Normal;
            }
            _ => {
                self.status_message = None;
                self.input_mode = InputMode::Normal;
            }
        }
        Ok(())
    }

    fn handle_edit_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.suggestion_index = None;
                self.status_message = Some("Edit cancelled.".to_string());
            }
            KeyCode::Down => {
                if self.suggestion_index.is_none() {
                    self.suggestions = if self.input_mode == InputMode::EditAccount {
                        self.build_account_suggestions()
                    } else {
                        self.build_project_suggestions()
                    };
                    if !self.suggestions.is_empty() {
                        self.suggestion_index = Some(0);
                    }
                } else if !self.suggestions.is_empty() {
                    let idx = self.suggestion_index.unwrap_or(0);
                    self.suggestion_index = Some((idx + 1) % self.suggestions.len());
                }
            }
            KeyCode::Up => {
                if let Some(idx) = self.suggestion_index {
                    if !self.suggestions.is_empty() {
                        self.suggestion_index = Some(if idx == 0 {
                            self.suggestions.len() - 1
                        } else {
                            idx - 1
                        });
                    }
                }
            }
            KeyCode::Enter => {
                if let Some(idx) = self.suggestion_index {
                    // Pick suggestion into buffer
                    if let Some(suggestion) = self.suggestions.get(idx) {
                        let suggestion = suggestion.clone();
                        let char_count = suggestion.chars().count();
                        if self.input_mode == InputMode::EditAccount {
                            self.edit_account_buffer = suggestion;
                        } else {
                            self.edit_project_buffer = suggestion;
                        }
                        self.edit_cursor_pos = char_count;
                    }
                    self.suggestion_index = None;
                } else {
                    // Save and exit edit mode
                    self.save_edit()?;
                }
            }
            KeyCode::Tab => {
                if self.input_mode == InputMode::EditAccount {
                    self.input_mode = InputMode::EditProject;
                    self.edit_cursor_pos = self.edit_project_buffer.chars().count();
                    self.suggestion_index = None;
                    let account = self.edit_account_buffer.trim().to_string();
                    self.start_project_fetch(&account);
                } else {
                    self.save_edit()?;
                }
            }
            KeyCode::Left => {
                if self.edit_cursor_pos > 0 {
                    self.edit_cursor_pos -= 1;
                }
            }
            KeyCode::Right => {
                let buf = if self.input_mode == InputMode::EditAccount {
                    &self.edit_account_buffer
                } else {
                    &self.edit_project_buffer
                };
                if self.edit_cursor_pos < buf.chars().count() {
                    self.edit_cursor_pos += 1;
                }
            }
            KeyCode::Home => {
                self.edit_cursor_pos = 0;
            }
            KeyCode::End => {
                let buf = if self.input_mode == InputMode::EditAccount {
                    &self.edit_account_buffer
                } else {
                    &self.edit_project_buffer
                };
                self.edit_cursor_pos = buf.chars().count();
            }
            KeyCode::Backspace => {
                if self.edit_cursor_pos > 0 {
                    let buf = if self.input_mode == InputMode::EditAccount {
                        &mut self.edit_account_buffer
                    } else {
                        &mut self.edit_project_buffer
                    };
                    let byte_idx = buf.char_indices()
                        .nth(self.edit_cursor_pos - 1)
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    buf.remove(byte_idx);
                    self.edit_cursor_pos -= 1;
                }
                self.suggestion_index = None;
            }
            KeyCode::Delete => {
                let buf = if self.input_mode == InputMode::EditAccount {
                    &mut self.edit_account_buffer
                } else {
                    &mut self.edit_project_buffer
                };
                if self.edit_cursor_pos < buf.chars().count() {
                    let byte_idx = buf.char_indices()
                        .nth(self.edit_cursor_pos)
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    buf.remove(byte_idx);
                }
                self.suggestion_index = None;
            }
            KeyCode::Char(c) => {
                let is_valid = if self.input_mode == InputMode::EditAccount {
                    // Email characters: letters, digits, @, ., -, _, +
                    c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '-' | '_' | '+')
                } else {
                    // GCP project: first char must be a letter, rest: letters, digits, -, _
                    if self.edit_cursor_pos == 0 {
                        c.is_ascii_alphabetic()
                    } else {
                        c.is_ascii_alphanumeric() || c == '-' || c == '_'
                    }
                };
                if is_valid {
                    let buf = if self.input_mode == InputMode::EditAccount {
                        &mut self.edit_account_buffer
                    } else {
                        &mut self.edit_project_buffer
                    };
                    let byte_idx = buf.char_indices()
                        .nth(self.edit_cursor_pos)
                        .map(|(i, _)| i)
                        .unwrap_or(buf.len());
                    buf.insert(byte_idx, c);
                    self.edit_cursor_pos += 1;
                }
                self.suggestion_index = None;
            }
            _ => {}
        }
        Ok(())
    }

    fn build_account_suggestions(&self) -> Vec<String> {
        let mut seen = std::collections::BTreeSet::new();
        for profile in &self.profiles {
            if !profile.user_account.is_empty() {
                seen.insert(profile.user_account.clone());
            }
            if !profile.adc_account.is_empty() {
                seen.insert(profile.adc_account.clone());
            }
        }
        if let Ok(auth_accounts) = gcloud::list_authenticated_accounts() {
            for account in auth_accounts {
                seen.insert(account);
            }
        }
        seen.into_iter().collect()
    }

    fn build_project_suggestions(&self) -> Vec<String> {
        let mut seen = std::collections::BTreeSet::new();
        for project in &self.fetched_projects {
            seen.insert(project.clone());
        }
        for profile in &self.profiles {
            if !profile.user_project.is_empty() {
                seen.insert(profile.user_project.clone());
            }
            if !profile.adc_quota_project.is_empty() {
                seen.insert(profile.adc_quota_project.clone());
            }
        }
        seen.into_iter().collect()
    }

    /// Save the edited part. The gcloud state follows the profile at once: the user part's
    /// configuration is rewritten when sync is on, and a changed quota project is stamped into
    /// the stored ADC credential (and the live ADC file when it is this profile's).
    fn save_edit(&mut self) -> Result<()> {
        let name = self.profile_names[self.selected_row].clone();
        let old = self.profiles[self.selected_row].clone();
        let account = self.edit_account_buffer.trim().to_string();
        let project = self.edit_project_buffer.trim().to_string();
        if account.is_empty() {
            self.status_message = Some("Account is required.".to_string());
            return Ok(());
        }

        let mut profile = old.clone();
        match self.edit_col {
            Column::User => {
                profile.user_account = account;
                profile.user_project = project;
            }
            Column::Adc => {
                profile.adc_account = account;
                profile.adc_quota_project = project;
            }
            Column::Both => unreachable!("edit_col is mapped to User or Adc before edit mode"),
        }
        self.store.add_profile(&name, profile.clone())?;

        let mut note = String::new();
        match self.edit_col {
            Column::User => {
                let changed = profile.user_account != old.user_account
                    || profile.user_project != old.user_project;
                if changed && matches!(self.sync_mode, SyncMode::Strict | SyncMode::Add) {
                    note = match gcloud::write_configuration(
                        &name,
                        &profile.user_account,
                        &profile.user_project,
                    ) {
                        Ok(()) => " gcloud configuration updated.".to_string(),
                        Err(e) => format!(" Failed to update gcloud configuration: {:#}", e),
                    };
                }
            }
            Column::Adc => {
                if profile.adc_quota_project != old.adc_quota_project && self.store.has_adc(&name) {
                    let result = if self.active_adc.as_deref() == Some(name.as_str()) {
                        gcloud::activate_adc(&self.store, &name, &profile.adc_quota_project)
                    } else {
                        gcloud::update_stored_quota_project(
                            &self.store,
                            &name,
                            &profile.adc_quota_project,
                        )
                    };
                    note = match result {
                        Ok(()) => " ADC quota project applied.".to_string(),
                        Err(e) => format!(" Failed to apply ADC quota project: {:#}", e),
                    };
                }
            }
            Column::Both => {}
        }

        self.reload()?;
        self.input_mode = InputMode::Normal;
        self.suggestion_index = None;
        self.status_message = Some(format!("Profile '{}' updated.{}", name, note));
        Ok(())
    }

    /// Whether one part of the selected profile has valid credentials: the background result
    /// when it is in, otherwise a check now.
    fn selected_part_valid(&self, user: bool) -> bool {
        let cached = if user {
            &self.user_auth_valid
        } else {
            &self.adc_auth_valid
        };
        if let Some(valid) = cached.get(self.selected_row).copied().flatten() {
            return valid;
        }
        let name = &self.profile_names[self.selected_row];
        let profile = &self.profiles[self.selected_row];
        if user {
            gcloud::check_account_auth(&profile.user_account)
        } else {
            gcloud::check_adc_auth(&self.store, name, profile)
        }
    }

    fn activate_selected(&mut self) -> Result<()> {
        let parts = self.selected_col.parts();
        let needing = Parts {
            user: parts.user && !self.selected_part_valid(true),
            adc: parts.adc && !self.selected_part_valid(false),
        };
        // Defer to the main loop if an interactive login is needed
        if needing.any() {
            self.pending_action = PendingAction::ReauthAndActivate { needing };
            return Ok(());
        }
        self.do_activate()
    }

    /// Activate the selected column's parts of the selected profile.
    fn do_activate(&mut self) -> Result<()> {
        let name = self.profile_names[self.selected_row].clone();
        let profile = self.profiles[self.selected_row].clone();
        let parts = self.selected_col.parts();

        gcloud::activate(&self.store, &name, &profile, parts)?;

        if parts.user {
            self.active_profile = Some(name.clone());
            let mut data = self.store.load_profiles()?;
            data.active_profile = Some(name.clone());
            self.store.save_profiles(&data)?;
        }
        self.active_adc = gcloud::active_adc_profile(&self.store, self.profile_names.iter())?;
        self.status_message = Some(format!("Activated {}.", parts.describe(&name)));
        Ok(())
    }

    /// Run a pending action's interactive gcloud commands; the caller has suspended the TUI.
    /// The profile list is reloaded afterwards whether or not the action succeeded.
    pub fn execute_pending(&mut self, pending: PendingAction) -> Result<()> {
        let name = self.profile_names[self.selected_row].clone();
        let profile = self.profiles[self.selected_row].clone();
        let parts = self.selected_col.parts();

        let result = match pending {
            PendingAction::None => Ok(()),
            PendingAction::Reauth => {
                let all = self.store.load_profiles()?.profiles;
                let result = gcloud::authenticate_only(&self.store, &all, &name, parts);
                if result.is_ok() {
                    self.status_message =
                        Some(format!("Authenticated {}.", parts.describe(&name)));
                }
                result
            }
            PendingAction::ReauthAndActivate { needing } => {
                match gcloud::authenticate(&self.store, &name, &profile, needing) {
                    Ok(()) => self.do_activate(),
                    Err(e) => Err(e),
                }
            }
        };

        self.reload()?;
        result
    }
}
