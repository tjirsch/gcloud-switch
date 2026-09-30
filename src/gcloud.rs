use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::profile::Profile;
use crate::store::{self, Store};

/// Which parts of a profile an action targets: the gcloud user configuration, the
/// Application Default Credentials (ADC), or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Parts {
    pub user: bool,
    pub adc: bool,
}

impl Parts {
    pub const BOTH: Parts = Parts { user: true, adc: true };
    pub const USER: Parts = Parts { user: true, adc: false };
    pub const ADC: Parts = Parts { user: false, adc: true };

    pub fn any(self) -> bool {
        self.user || self.adc
    }

    /// "profile 'x'", "user configuration of 'x'" or "ADC of 'x'", for status messages.
    pub fn describe(self, name: &str) -> String {
        match (self.user, self.adc) {
            (true, true) => format!("profile '{}'", name),
            (true, false) => format!("user configuration of '{}'", name),
            (false, true) => format!("ADC of '{}'", name),
            (false, false) => format!("nothing of '{}'", name),
        }
    }

    /// Short marker for tables: "both", "user", "adc" or "-".
    pub fn marker(self) -> &'static str {
        match (self.user, self.adc) {
            (true, true) => "both",
            (true, false) => "user",
            (false, true) => "adc",
            (false, false) => "-",
        }
    }
}

fn gcloud_config_dir() -> Result<PathBuf> {
    // gcloud always uses ~/.config/gcloud on all platforms, ignoring XDG/macOS conventions,
    // unless CLOUDSDK_CONFIG is set.
    if let Ok(custom) = std::env::var("CLOUDSDK_CONFIG") {
        return Ok(PathBuf::from(custom));
    }
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".config").join("gcloud"))
}

/// The ADC file client libraries read: gcloud's well-known location.
fn live_adc_path() -> Result<PathBuf> {
    Ok(gcloud_config_dir()?.join("application_default_credentials.json"))
}

/// Read gcloud's currently active configuration name.
pub fn read_active_config() -> Result<Option<String>> {
    let path = gcloud_config_dir()?.join("active_config");
    if !path.exists() {
        return Ok(None);
    }
    let name = fs::read_to_string(&path)?.trim().to_string();
    if name.is_empty() {
        Ok(None)
    } else {
        Ok(Some(name))
    }
}

fn configurations_dir() -> Result<PathBuf> {
    let dir = gcloud_config_dir()?.join("configurations");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Run a gcloud command whose output is not needed. A non-zero exit becomes an error that
/// carries gcloud's stderr.
fn run_gcloud(args: &[&str]) -> Result<()> {
    let output = Command::new("gcloud")
        .args(args)
        .stdout(Stdio::null())
        .output()
        .with_context(|| {
            format!(
                "Failed to run gcloud {} (is gcloud installed and in PATH?)",
                args.join(" ")
            )
        })?;
    if !output.status.success() {
        anyhow::bail!(
            "gcloud {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Run an interactive gcloud command (browser login) with the terminal inherited.
fn run_gcloud_interactive(args: &[&str]) -> Result<()> {
    let status = Command::new("gcloud")
        .args(args)
        .status()
        .with_context(|| format!("Failed to run gcloud {}", args.join(" ")))?;
    if !status.success() {
        anyhow::bail!("gcloud {} failed with status {}", args.join(" "), status);
    }
    Ok(())
}

/// Create the gcloud configuration `name` if it does not exist and set its account and
/// project. An empty project unsets `core/project`. Every property is written with
/// `--configuration=<name>`, so this never depends on, or changes, the active configuration.
pub fn write_configuration(name: &str, account: &str, project: &str) -> Result<()> {
    if account.is_empty() {
        anyhow::bail!(
            "Profile '{}' has no user account; a gcloud configuration needs one.",
            name
        );
    }

    // Create fails when the configuration already exists, which is fine.
    let status = Command::new("gcloud")
        .args(["config", "configurations", "create", name, "--no-activate"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("Failed to run gcloud (is it installed and in PATH?)")?;
    let config_file = configurations_dir()?.join(format!("config_{}", name));
    if !config_file.exists() && !status.success() {
        anyhow::bail!(
            "Failed to create gcloud configuration '{}'. Check that the gcloud CLI is working.",
            name
        );
    }

    let configuration = format!("--configuration={}", name);
    run_gcloud(&["config", "set", "account", account, &configuration])?;
    if project.is_empty() {
        run_gcloud(&["config", "unset", "project", &configuration])
    } else {
        run_gcloud(&["config", "set", "project", project, &configuration])
    }
}

/// Delete a gcloud configuration.
pub fn delete_configuration(name: &str) -> Result<()> {
    let _ = Command::new("gcloud")
        .args(["config", "configurations", "delete", name, "--quiet"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    Ok(())
}

/// Activate a profile's user part: write its configuration, then make it gcloud's active one.
pub fn activate_user(name: &str, account: &str, project: &str) -> Result<()> {
    write_configuration(name, account, project)?;
    run_gcloud(&["config", "configurations", "activate", name])
}

/// Set or remove the `quota_project_id` of an ADC document. Empty means no quota project.
pub fn stamp_quota_project(adc: &mut Value, quota_project: &str) -> Result<()> {
    let object = adc
        .as_object_mut()
        .ok_or_else(|| anyhow!("ADC credential is not a JSON object"))?;
    if quota_project.is_empty() {
        object.remove("quota_project_id");
    } else {
        object.insert(
            "quota_project_id".to_string(),
            Value::String(quota_project.to_string()),
        );
    }
    Ok(())
}

/// The object holding the user OAuth fields (`client_id`, `client_secret`, `refresh_token`):
/// the document itself for `authorized_user`, `source_credentials` for
/// `impersonated_service_account` (what ADC login writes when the configuration has
/// `auth/impersonate_service_account`). `None` for any other credential type.
fn adc_user_creds(adc: &Value) -> Option<&Value> {
    match adc.get("type").and_then(Value::as_str) {
        Some("authorized_user") => Some(adc),
        Some("impersonated_service_account") => adc.get("source_credentials"),
        _ => None,
    }
}

/// A profile's stored ADC credential with the quota project applied. The stored file is the
/// source of truth, so it is rewritten when the stamp changed it.
fn stored_adc_with_quota(store: &Store, name: &str, quota_project: &str) -> Result<Value> {
    let mut adc = store.load_adc_json(name)?.ok_or_else(|| {
        anyhow!(
            "No ADC credential stored for profile '{}'. Authenticate its ADC part first.",
            name
        )
    })?;
    let before = adc.clone();
    stamp_quota_project(&mut adc, quota_project)?;
    if adc != before {
        store.save_adc_json(name, &adc)?;
    }
    Ok(adc)
}

/// Apply a changed quota project to a profile's stored ADC credential.
pub fn update_stored_quota_project(store: &Store, name: &str, quota_project: &str) -> Result<()> {
    stored_adc_with_quota(store, name, quota_project).map(|_| ())
}

/// Activate a profile's ADC part: its stored credential, stamped with the profile's quota
/// project, becomes the live ADC file.
pub fn activate_adc(store: &Store, name: &str, quota_project: &str) -> Result<()> {
    let adc = stored_adc_with_quota(store, name, quota_project)?;
    store::write_adc_file(&live_adc_path()?, &adc)
}

/// Activate the requested parts of a profile.
pub fn activate(store: &Store, name: &str, profile: &Profile, parts: Parts) -> Result<()> {
    if parts.user {
        activate_user(name, &profile.user_account, &profile.user_project)?;
    }
    if parts.adc {
        activate_adc(store, name, &profile.adc_quota_project)?;
    }
    Ok(())
}

/// "project p" or "no project".
fn describe_project(project: &str) -> String {
    if project.is_empty() {
        "no project".to_string()
    } else {
        format!("project {}", project)
    }
}

/// "quota project q" or "no quota project".
fn describe_quota_project(quota_project: &str) -> String {
    if quota_project.is_empty() {
        "no quota project".to_string()
    } else {
        format!("quota project {}", quota_project)
    }
}

/// The status line after an activation: which parts, and the projects they now carry.
pub fn activation_message(name: &str, profile: &Profile, parts: Parts) -> String {
    match (parts.user, parts.adc) {
        (true, true) => format!(
            "Activated profile '{}': {}, ADC {}.",
            name,
            describe_project(&profile.user_project),
            describe_quota_project(&profile.adc_quota_project)
        ),
        (true, false) => format!(
            "Activated user configuration of '{}': {}.",
            name,
            describe_project(&profile.user_project)
        ),
        (false, true) => format!(
            "Activated ADC of '{}': {}.",
            name,
            describe_quota_project(&profile.adc_quota_project)
        ),
        (false, false) => format!("Activated nothing of '{}'.", name),
    }
}

/// Run `gcloud auth login` for the account. gcloud verifies that the browser signed in as
/// that account and refuses otherwise. It never changes the active configuration. With
/// `update_adc`, gcloud also writes the credential to the live ADC file; the caller reads it.
fn login_user(account: &str, update_adc: bool) -> Result<()> {
    let mut args = vec![
        "auth",
        "login",
        account,
        "--no-activate",
        "--force",
        "--verbosity=error",
    ];
    if update_adc {
        args.push("--update-adc");
    }
    run_gcloud_interactive(&args)
}

/// The live ADC file's bytes, taken out of the way: gcloud skips an ADC login for an account
/// the live file already names, valid or not, so the file must not be there during a login.
/// `restore` puts it back (or removes what a failed login left) when the login does not end
/// in a stored credential.
struct LiveAdcAside {
    path: PathBuf,
    previous: Option<Vec<u8>>,
}

impl LiveAdcAside {
    fn take() -> Result<Self> {
        let path = live_adc_path()?;
        let previous = fs::read(&path).ok();
        if previous.is_some() {
            fs::remove_file(&path)
                .with_context(|| format!("Failed to move {} aside", path.display()))?;
        }
        Ok(Self { path, previous })
    }

    fn restore(self) -> Result<()> {
        match self.previous {
            Some(bytes) => fs::write(&self.path, bytes)
                .with_context(|| format!("Failed to restore {}", self.path.display())),
            None if self.path.exists() => fs::remove_file(&self.path)
                .with_context(|| format!("Failed to remove {}", self.path.display())),
            None => Ok(()),
        }
    }
}

/// Read and parse the live ADC file after a login wrote it.
fn read_live_adc(path: &std::path::Path) -> Result<Value> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("gcloud did not write {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("{} is not valid JSON", path.display()))
}

/// Run `gcloud auth application-default login` for the account and return the credential it
/// wrote. gcloud verifies that the browser signed in as that account, refuses otherwise, and
/// records the account in the file. The quota project is left to the caller (gcloud would take
/// it from the active configuration, which is another profile's during a switch). Whatever
/// was live before is put back when the login fails.
fn login_adc(account: &str) -> Result<Value> {
    let aside = LiveAdcAside::take()?;
    let result = run_gcloud_interactive(&[
        "auth",
        "application-default",
        "login",
        account,
        "--disable-quota-project",
        "--quiet",
        "--verbosity=error",
    ])
    .and_then(|()| read_live_adc(&aside.path))
    .and_then(|adc| {
        if adc_account_matches(&adc, account) {
            Ok(adc)
        } else {
            Err(anyhow!(
                "The ADC login signed in as {}, not as {}. Nothing was stored.",
                adc["account"].as_str().unwrap_or("?"),
                account
            ))
        }
    });
    if result.is_err() {
        aside.restore()?;
    }
    result
}

/// An ADC document built from the account's gcloud user credential in `credentials.db`: the
/// same document `gcloud auth login --update-adc` writes, so no browser is needed while that
/// credential is valid.
fn adc_from_user_credential(account: &str) -> Result<Value> {
    let creds = read_gcloud_credentials(account)?
        .ok_or_else(|| anyhow!("gcloud holds no credential for {}", account))?;
    match creds.get("type").and_then(Value::as_str) {
        Some("authorized_user") => {}
        other => anyhow::bail!(
            "The gcloud credential of {} is of type {}, not authorized_user; only a user login can serve as ADC.",
            account,
            other.unwrap_or("unknown")
        ),
    }
    let field = |key: &str| -> Result<Value> {
        creds
            .get(key)
            .cloned()
            .ok_or_else(|| anyhow!("The gcloud credential of {} has no {}", account, key))
    };
    let mut adc = json!({
        "type": "authorized_user",
        "client_id": field("client_id")?,
        "client_secret": field("client_secret")?,
        "refresh_token": field("refresh_token")?,
    });
    if let Some(universe) = creds.get("universe_domain") {
        adc["universe_domain"] = universe.clone();
    }
    Ok(adc)
}

/// Record the account and the quota project in a fresh ADC credential, store it for the
/// profile, write it live and say so. `origin` says where the credential came from.
fn store_adc(store: &Store, name: &str, profile: &Profile, mut adc: Value, origin: &str) -> Result<()> {
    let object = adc
        .as_object_mut()
        .ok_or_else(|| anyhow!("ADC credential is not a JSON object"))?;
    object.insert(
        "account".to_string(),
        Value::String(profile.adc_account.clone()),
    );
    stamp_quota_project(&mut adc, &profile.adc_quota_project)?;
    store.save_adc_json(name, &adc)?;
    store::write_adc_file(&live_adc_path()?, &adc)?;
    println!(
        "ADC of profile '{}' {} for {}, {}.",
        name,
        origin,
        profile.adc_account,
        describe_quota_project(&profile.adc_quota_project)
    );
    Ok(())
}

/// Interactive login for the requested parts: the ones the caller found invalid, or the ones
/// the user asked to re-authenticate. Three shapes, by what is needed and whether the profile
/// uses one account for both parts:
///
/// 1. both parts, one account: one browser login (`gcloud auth login --update-adc`) yields
///    the user credential and the ADC.
/// 2. the ADC alone, one account, and that account's user credential is valid: the ADC is
///    derived from the user credential; no browser.
/// 3. otherwise: `gcloud auth login` for the user part, and an ADC login with the profile's
///    ADC account, which gcloud verifies against the browser.
///
/// Every stored ADC names its account, carries the profile's quota project and is written
/// live as well. Callers that only authenticate use `authenticate_only`, which puts the
/// previously live ADC back afterwards.
pub fn authenticate(store: &Store, name: &str, profile: &Profile, parts: Parts) -> Result<()> {
    let one_account = profile.adc_account == profile.user_account;
    if parts.user && parts.adc && one_account {
        let aside = LiveAdcAside::take()?;
        let adc = login_user(&profile.user_account, true).and_then(|()| read_live_adc(&aside.path));
        let adc = match adc {
            Ok(adc) => adc,
            Err(e) => {
                aside.restore()?;
                return Err(e);
            }
        };
        return store_adc(store, name, profile, adc, "stored from the same login");
    }
    if parts.user {
        login_user(&profile.user_account, false)?;
    }
    if parts.adc {
        if one_account && check_account_auth(&profile.user_account) {
            let adc = adc_from_user_credential(&profile.user_account)?;
            store_adc(store, name, profile, adc, "derived from the gcloud login (no browser)")?;
        } else {
            let adc = login_adc(&profile.adc_account)?;
            store_adc(store, name, profile, adc, "stored")?;
        }
    }
    Ok(())
}

/// Authenticate without activating. An ADC login replaces the live ADC file, so when another
/// profile's ADC was live before, it is put back afterwards.
pub fn authenticate_only(
    store: &Store,
    all: &BTreeMap<String, Profile>,
    active_profile: Option<&str>,
    name: &str,
    parts: Parts,
) -> Result<()> {
    let profile = all
        .get(name)
        .ok_or_else(|| anyhow!("Profile '{}' not found", name))?;
    let previous_adc = if parts.adc {
        active_adc_profile(store, all.keys(), active_profile)?
    } else {
        None
    };
    authenticate(store, name, profile, parts)?;
    if let Some(previous) = previous_adc.filter(|p| p != name) {
        let previous_profile = &all[&previous];
        activate_adc(store, &previous, &previous_profile.adc_quota_project)
            .with_context(|| format!("Restoring the ADC of '{}'", previous))?;
    }
    Ok(())
}

/// The live ADC document, or `None` when there is no live file or it is not JSON.
fn read_live_adc_if_any() -> Result<Option<Value>> {
    let live = live_adc_path()?;
    if !live.exists() {
        return Ok(None);
    }
    // The live file belongs to gcloud; if it is not an ADC document, no profile's ADC is live.
    Ok(serde_json::from_str(&fs::read_to_string(&live)?).ok())
}

/// The profile whose ADC is live. Profiles that share one account also share one refresh
/// token (an ADC derived from the user credential), so the candidates are the profiles whose
/// stored credential equals the live file in refresh token AND quota project; among several,
/// the active profile, else the first by name. Profiles that agree in both write the same
/// live file, so the rest is a name. `None` when the live file is missing, is not a user
/// credential, or matches no profile.
pub fn active_adc_profile<'a>(
    store: &Store,
    names: impl IntoIterator<Item = &'a String>,
    active_profile: Option<&str>,
) -> Result<Option<String>> {
    let Some(live) = read_live_adc_if_any()? else {
        return Ok(None);
    };
    let mut stored = Vec::new();
    for name in names {
        if let Some(adc) = store.load_adc_json(name)? {
            stored.push((name.clone(), adc));
        }
    }
    Ok(match_adc_profile(&live, &stored, active_profile))
}

/// The matching rule of `active_adc_profile`, over the stored credentials in name order.
fn match_adc_profile(
    live: &Value,
    stored: &[(String, Value)],
    active_profile: Option<&str>,
) -> Option<String> {
    let live_token = refresh_token(live)?;
    let live_quota = quota_project(live);
    let matches =
        |adc: &Value| refresh_token(adc) == Some(live_token) && quota_project(adc) == live_quota;
    match active_profile {
        Some(active) if stored.iter().any(|(name, adc)| name == active && matches(adc)) => {
            Some(active.to_string())
        }
        _ => stored
            .iter()
            .find(|(_, adc)| matches(adc))
            .map(|(name, _)| name.clone()),
    }
}

fn refresh_token(adc: &Value) -> Option<&str> {
    adc_user_creds(adc)?.get("refresh_token")?.as_str()
}

fn quota_project(adc: &Value) -> Option<&str> {
    adc.get("quota_project_id")?.as_str()
}

/// gcloud's active configuration as it is on disk: name, account and project (empty when
/// unset), and the profile of that name with whether the configuration has drifted from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveConfiguration {
    pub name: String,
    pub account: String,
    pub project: String,
    /// The profile of the same name, when there is one.
    pub profile: Option<String>,
    /// The configuration's account or project is not what that profile says.
    pub differs: bool,
}

/// The live ADC file as it is on disk: the account it records (empty when gcloud did not
/// record one), its quota project, and the profile it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAdc {
    pub account: String,
    pub quota_project: String,
    pub profile: Option<String>,
    /// The recorded account is not what that profile says.
    pub differs: bool,
}

/// What gcloud holds right now, read from its files rather than from the profiles.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LiveState {
    /// `None` when gcloud has no active configuration.
    pub configuration: Option<LiveConfiguration>,
    /// `None` when there is no live ADC file.
    pub adc: Option<LiveAdc>,
}

impl LiveConfiguration {
    /// "name · account · project p", for a status line.
    pub fn describe(&self) -> String {
        format!(
            "{} \u{00b7} {} \u{00b7} {}",
            self.name,
            if self.account.is_empty() { "no account" } else { self.account.as_str() },
            describe_project(&self.project)
        )
    }

    /// What the profiles do not say about this configuration, if anything.
    pub fn drift_note(&self) -> Option<String> {
        match &self.profile {
            Some(profile) if self.differs => Some(format!("differs from profile '{}'", profile)),
            Some(_) => None,
            None => Some("no profile".to_string()),
        }
    }
}

impl LiveAdc {
    /// "profile · account · quota project q", for a status line.
    pub fn describe(&self) -> String {
        format!(
            "{} \u{00b7} {} \u{00b7} {}",
            self.profile.as_deref().unwrap_or("no profile"),
            if self.account.is_empty() { "account not recorded" } else { self.account.as_str() },
            describe_quota_project(&self.quota_project)
        )
    }

    /// What the profile does not say about this credential, if anything.
    pub fn drift_note(&self) -> Option<String> {
        self.differs.then(|| "differs from the profile".to_string())
    }
}

/// Read the live state from gcloud's files.
pub fn live_state(
    store: &Store,
    profiles: &BTreeMap<String, Profile>,
    active_profile: Option<&str>,
) -> Result<LiveState> {
    let configuration = match read_active_config()? {
        Some(name) => {
            let (account, project) = read_configuration(&name)?.unwrap_or_default();
            let profile = profiles.get(&name);
            let differs = profile
                .map(|p| p.user_account != account || p.user_project != project)
                .unwrap_or(false);
            Some(LiveConfiguration {
                profile: profile.map(|_| name.clone()),
                name,
                account,
                project,
                differs,
            })
        }
        None => None,
    };
    let adc = match read_live_adc_if_any()? {
        Some(live) => {
            let account = live
                .get("account")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let quota = quota_project(&live).unwrap_or_default().to_string();
            let profile = active_adc_profile(store, profiles.keys(), active_profile)?;
            let differs = profile
                .as_ref()
                .and_then(|name| profiles.get(name))
                .map(|p| !account.is_empty() && p.adc_account != account)
                .unwrap_or(false);
            Some(LiveAdc {
                account,
                quota_project: quota,
                profile,
                differs,
            })
        }
        None => None,
    };
    Ok(LiveState { configuration, adc })
}

/// Validity of the live user credential: the active configuration's account has a credential
/// in gcloud that Google still accepts. `None` when there is no configuration or account.
pub fn check_live_user_auth(state: &LiveState) -> Option<bool> {
    let account = &state.configuration.as_ref()?.account;
    if account.is_empty() {
        return None;
    }
    Some(check_account_auth(account))
}

/// Validity of the live ADC credential. `None` when there is no live file.
pub fn check_live_adc_auth(state: &LiveState) -> Result<Option<bool>> {
    let Some(adc) = &state.adc else {
        return Ok(None);
    };
    Ok(Some(check_adc_auth_at(live_adc_path()?, adc.account.clone())))
}

/// Whether an ADC document may serve `expected_account`. gcloud writes the `account` field
/// empty in most flows; only a recorded, different account is a mismatch.
fn adc_account_matches(adc: &Value, expected_account: &str) -> bool {
    match adc.get("account").and_then(Value::as_str) {
        Some(account) if !account.is_empty() => account == expected_account,
        _ => true,
    }
}

/// Validate a stored ADC credential file: it must exist, belong to `expected_account` when it
/// records one, and hold a refresh token Google still accepts. Credential types that cannot
/// be validated locally count as valid. Takes owned arguments so it can run on a thread.
pub fn check_adc_auth_at(path: PathBuf, expected_account: String) -> bool {
    let Ok(content) = fs::read_to_string(&path) else {
        return false;
    };
    let Ok(adc) = serde_json::from_str::<Value>(&content) else {
        return false;
    };
    if !adc_account_matches(&adc, &expected_account) {
        return false;
    }
    match adc_user_creds(&adc) {
        Some(creds) => validate_token_blocking(creds).unwrap_or(false),
        None => true,
    }
}

/// Validate the stored ADC credential of a profile.
pub fn check_adc_auth(store: &Store, name: &str, profile: &Profile) -> bool {
    check_adc_auth_at(store.adc_path(name), profile.adc_account.clone())
}

/// Which of the requested parts currently lack valid credentials.
pub fn parts_needing_auth(store: &Store, name: &str, profile: &Profile, parts: Parts) -> Parts {
    Parts {
        user: parts.user && !check_account_auth(&profile.user_account),
        adc: parts.adc && !check_adc_auth(store, name, profile),
    }
}

/// List projects accessible by a given account via `gcloud projects list`.
pub fn list_projects_for_account(account: &str) -> Result<Vec<String>> {
    let output = Command::new("gcloud")
        .args([
            "projects",
            "list",
            &format!("--account={}", account),
            "--format=value(projectId)",
            "--sort-by=projectId",
        ])
        .output()
        .context("Failed to run gcloud projects list")?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Read credentials for an account from gcloud's credentials.db.
pub fn read_gcloud_credentials(account: &str) -> Result<Option<Value>> {
    let db_path = gcloud_config_dir()?.join("credentials.db");
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("Failed to open credentials.db at {}", db_path.display()))?;
    let mut stmt = conn.prepare("SELECT value FROM credentials WHERE account_id = ?1")?;
    let result: Option<String> = stmt
        .query_row(rusqlite::params![account], |row| row.get(0))
        .ok();
    match result {
        Some(blob) => {
            let value: Value =
                serde_json::from_str(&blob).context("Failed to parse credentials blob as JSON")?;
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

/// Validate a refresh token by attempting a token exchange.
pub fn validate_token_blocking(credentials: &Value) -> Result<bool> {
    let client_id = credentials
        .get("client_id")
        .and_then(|v| v.as_str())
        .context("credentials missing client_id")?;
    let client_secret = credentials
        .get("client_secret")
        .and_then(|v| v.as_str())
        .context("credentials missing client_secret")?;
    let refresh_token = credentials
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .context("credentials missing refresh_token")?;
    let token_uri = credentials
        .get("token_uri")
        .and_then(|v| v.as_str())
        .unwrap_or("https://oauth2.googleapis.com/token");

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let resp = client
        .post(token_uri)
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .send()?;

    Ok(resp.status().is_success())
}

/// Check whether an account's gcloud credentials are valid.
/// Returns false on any error (missing from DB, invalid token, network issue).
/// Runs the blocking HTTP call on a dedicated thread to keep the main thread free.
pub fn check_account_auth(account: &str) -> bool {
    let creds = match read_gcloud_credentials(account) {
        Ok(Some(c)) => c,
        _ => return false,
    };
    std::thread::spawn(move || validate_token_blocking(&creds).unwrap_or(false))
        .join()
        .unwrap_or(false)
}

/// List all account emails that have stored credentials in credentials.db.
pub fn list_authenticated_accounts() -> Result<Vec<String>> {
    let db_path = gcloud_config_dir()?.join("credentials.db");
    if !db_path.exists() {
        return Ok(Vec::new());
    }
    let conn = Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .with_context(|| format!("Failed to open credentials.db at {}", db_path.display()))?;
    let mut stmt = conn.prepare("SELECT account_id FROM credentials")?;
    let accounts: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(accounts)
}

/// The account and project a configuration file sets; empty when it does not set them.
fn parse_configuration(content: &str) -> (String, String) {
    let mut account = String::new();
    let mut project = String::new();
    for line in content.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("account = ") {
            account = val.trim().to_string();
        }
        if let Some(val) = line.strip_prefix("project = ") {
            project = val.trim().to_string();
        }
    }
    (account, project)
}

/// The account and project of the configuration `name`, or `None` when it has no file.
pub fn read_configuration(name: &str) -> Result<Option<(String, String)>> {
    let path = configurations_dir()?.join(format!("config_{}", name));
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    Ok(Some(parse_configuration(&content)))
}

/// All gcloud configurations as (name, account, project); account and project are empty when
/// the configuration does not set them.
pub fn discover_existing_configs() -> Result<Vec<(String, String, String)>> {
    let dir = match configurations_dir() {
        Ok(d) => d,
        Err(_) => return Ok(vec![]),
    };

    let mut results = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            if let Some(name) = file_name.strip_prefix("config_") {
                if let Ok(content) = fs::read_to_string(entry.path()) {
                    let (account, project) = parse_configuration(&content);
                    results.push((name.to_string(), account, project));
                }
            }
        }
    }
    Ok(results)
}

/// The gcloud configurations that can become profiles: those with an account. A profile
/// requires one, so configurations without are skipped on import.
pub fn importable_configs() -> Result<Vec<(String, String, String)>> {
    Ok(discover_existing_configs()?
        .into_iter()
        .filter(|(_, account, _)| !account.is_empty())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stamp_sets_replaces_and_removes_the_quota_project() {
        let mut adc = json!({"type": "authorized_user", "refresh_token": "r"});
        stamp_quota_project(&mut adc, "p1").unwrap();
        assert_eq!(adc["quota_project_id"], "p1");
        stamp_quota_project(&mut adc, "p2").unwrap();
        assert_eq!(adc["quota_project_id"], "p2");
        stamp_quota_project(&mut adc, "").unwrap();
        assert!(adc.get("quota_project_id").is_none());
        // Removing an absent key leaves the document untouched.
        stamp_quota_project(&mut adc, "").unwrap();
        assert_eq!(adc, json!({"type": "authorized_user", "refresh_token": "r"}));
    }

    #[test]
    fn stamp_rejects_a_non_object() {
        assert!(stamp_quota_project(&mut json!([]), "p").is_err());
    }

    #[test]
    fn refresh_token_follows_the_credential_type() {
        let user = json!({"type": "authorized_user", "refresh_token": "r"});
        assert_eq!(refresh_token(&user), Some("r"));
        let impersonated = json!({
            "type": "impersonated_service_account",
            "source_credentials": {"type": "authorized_user", "refresh_token": "s"}
        });
        assert_eq!(refresh_token(&impersonated), Some("s"));
        assert_eq!(refresh_token(&json!({"type": "external_account"})), None);
        assert_eq!(refresh_token(&json!({"refresh_token": "no type"})), None);
    }

    #[test]
    fn adc_account_matches_only_rejects_a_recorded_different_account() {
        let unrecorded = json!({"type": "authorized_user"});
        assert!(adc_account_matches(&unrecorded, "a@x.com"));
        let empty = json!({"type": "authorized_user", "account": ""});
        assert!(adc_account_matches(&empty, "a@x.com"));
        let same = json!({"type": "authorized_user", "account": "a@x.com"});
        assert!(adc_account_matches(&same, "a@x.com"));
        let other = json!({"type": "authorized_user", "account": "b@x.com"});
        assert!(!adc_account_matches(&other, "a@x.com"));
    }

    fn profile(user_project: &str, adc_quota_project: &str) -> Profile {
        Profile {
            user_account: "a@x.com".into(),
            user_project: user_project.into(),
            adc_account: "a@x.com".into(),
            adc_quota_project: adc_quota_project.into(),
            updated_at: None,
        }
    }

    #[test]
    fn activation_message_names_the_parts_and_their_projects() {
        let full = profile("p", "q");
        assert_eq!(
            activation_message("x", &full, Parts::BOTH),
            "Activated profile 'x': project p, ADC quota project q."
        );
        assert_eq!(
            activation_message("x", &full, Parts::USER),
            "Activated user configuration of 'x': project p."
        );
        assert_eq!(activation_message("x", &full, Parts::ADC), "Activated ADC of 'x': quota project q.");
        let bare = profile("", "");
        assert_eq!(
            activation_message("x", &bare, Parts::BOTH),
            "Activated profile 'x': no project, ADC no quota project."
        );
        assert_eq!(activation_message("x", &bare, Parts::ADC), "Activated ADC of 'x': no quota project.");
    }

    #[test]
    fn live_adc_matches_on_token_and_quota_project_and_prefers_the_active_profile() {
        let adc = |token: &str, quota: Option<&str>| match quota {
            Some(q) => json!({"type": "authorized_user", "refresh_token": token, "quota_project_id": q}),
            None => json!({"type": "authorized_user", "refresh_token": token}),
        };
        // Three profiles derived from one user login share the token; two share the quota project.
        let stored = vec![
            ("agentic".to_string(), adc("t", Some("p1"))),
            ("eri".to_string(), adc("t", Some("p2"))),
            ("gm".to_string(), adc("t", Some("p1"))),
            ("other".to_string(), adc("u", Some("p1"))),
        ];
        let live = adc("t", Some("p1"));
        assert_eq!(match_adc_profile(&live, &stored, None), Some("agentic".into()));
        assert_eq!(match_adc_profile(&live, &stored, Some("gm")), Some("gm".into()));
        // The active profile is only preferred among the candidates.
        assert_eq!(match_adc_profile(&live, &stored, Some("eri")), Some("agentic".into()));
        assert_eq!(match_adc_profile(&adc("t", Some("p2")), &stored, None), Some("eri".into()));
        // A quota project set by hand to another value matches no profile; so does no token.
        assert_eq!(match_adc_profile(&adc("t", Some("p3")), &stored, None), None);
        assert_eq!(match_adc_profile(&adc("t", None), &stored, None), None);
        assert_eq!(match_adc_profile(&json!({"type": "external_account"}), &stored, None), None);
    }

    #[test]
    fn live_descriptions_and_drift_notes() {
        let configuration = LiveConfiguration {
            name: "mmt01".into(),
            account: "a@x.com".into(),
            project: "p".into(),
            profile: Some("mmt01".into()),
            differs: false,
        };
        assert_eq!(configuration.describe(), "mmt01 \u{00b7} a@x.com \u{00b7} project p");
        assert_eq!(configuration.drift_note(), None);
        let drifted = LiveConfiguration { differs: true, ..configuration.clone() };
        assert_eq!(drifted.drift_note().as_deref(), Some("differs from profile 'mmt01'"));
        let orphan = LiveConfiguration { profile: None, account: String::new(), project: String::new(), ..configuration };
        assert_eq!(orphan.describe(), "mmt01 \u{00b7} no account \u{00b7} no project");
        assert_eq!(orphan.drift_note().as_deref(), Some("no profile"));

        let adc = LiveAdc { account: String::new(), quota_project: "q".into(), profile: None, differs: false };
        assert_eq!(adc.describe(), "no profile \u{00b7} account not recorded \u{00b7} quota project q");
        assert_eq!(adc.drift_note(), None);
        let recorded = LiveAdc { account: "b@x.com".into(), profile: Some("p".into()), differs: true, ..adc };
        assert_eq!(recorded.describe(), "p \u{00b7} b@x.com \u{00b7} quota project q");
        assert_eq!(recorded.drift_note().as_deref(), Some("differs from the profile"));
    }

    #[test]
    fn parts_describe_and_mark() {
        assert_eq!(Parts::BOTH.describe("x"), "profile 'x'");
        assert_eq!(Parts::USER.describe("x"), "user configuration of 'x'");
        assert_eq!(Parts::ADC.describe("x"), "ADC of 'x'");
        assert_eq!(Parts::BOTH.marker(), "both");
        assert_eq!(Parts::USER.marker(), "user");
        assert_eq!(Parts::ADC.marker(), "adc");
        assert_eq!(Parts::default().marker(), "-");
        assert!(!Parts::default().any());
    }
}
