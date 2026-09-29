use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use rusqlite::Connection;
use serde_json::Value;

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

/// Interactive login for the requested parts.
///
/// The user login never changes the active configuration. The ADC login writes gcloud's live
/// ADC file; that credential must belong to the profile's ADC account, is stamped with the
/// profile's quota project, stored for the profile and written back live. Callers that only
/// authenticate use `authenticate_only`, which puts the previously live ADC back afterwards.
pub fn authenticate(store: &Store, name: &str, profile: &Profile, parts: Parts) -> Result<()> {
    if parts.user {
        run_gcloud_interactive(&[
            "auth",
            "login",
            &profile.user_account,
            "--no-activate",
            "--force",
        ])?;
    }
    if parts.adc {
        // The account is deliberately not passed to gcloud: with it, gcloud skips the login
        // whenever the live ADC file already names that account, even when that credential
        // is expired. The account is checked below instead.
        let live = live_adc_path()?;
        let previous = fs::read(&live).ok();
        run_gcloud_interactive(&[
            "auth",
            "application-default",
            "login",
            "--disable-quota-project",
            "--quiet",
        ])?;
        let content = fs::read_to_string(&live).with_context(|| {
            format!(
                "gcloud auth application-default login did not write {}",
                live.display()
            )
        })?;
        let mut adc: Value = serde_json::from_str(&content)
            .with_context(|| format!("{} is not valid JSON", live.display()))?;
        if !adc_account_matches(&adc, &profile.adc_account) {
            // Undo gcloud's write so the wrong account's credential is not left live.
            match previous {
                Some(bytes) => fs::write(&live, bytes)?,
                None => fs::remove_file(&live)?,
            }
            anyhow::bail!(
                "The ADC login used account {}, but the ADC account of profile '{}' is {}. Nothing was stored.",
                adc["account"].as_str().unwrap_or("?"),
                name,
                profile.adc_account
            );
        }
        stamp_quota_project(&mut adc, &profile.adc_quota_project)?;
        store.save_adc_json(name, &adc)?;
        store::write_adc_file(&live, &adc)?;
    }
    Ok(())
}

/// Authenticate without activating. An ADC login replaces the live ADC file, so when another
/// profile's ADC was live before, it is put back afterwards.
pub fn authenticate_only(
    store: &Store,
    all: &BTreeMap<String, Profile>,
    name: &str,
    parts: Parts,
) -> Result<()> {
    let profile = all
        .get(name)
        .ok_or_else(|| anyhow!("Profile '{}' not found", name))?;
    let previous_adc = if parts.adc {
        active_adc_profile(store, all.keys())?
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

/// The profile whose ADC is live: the stored credential whose refresh token equals the live
/// ADC file's. `None` when the live file is missing, is not a user credential, or matches no
/// profile.
pub fn active_adc_profile<'a>(
    store: &Store,
    names: impl IntoIterator<Item = &'a String>,
) -> Result<Option<String>> {
    let live = live_adc_path()?;
    if !live.exists() {
        return Ok(None);
    }
    // The live file belongs to gcloud; if it is not an ADC document, no profile's ADC is live.
    let live: Value = match serde_json::from_str(&fs::read_to_string(&live)?) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let Some(live_token) = refresh_token(&live) else {
        return Ok(None);
    };
    for name in names {
        if let Some(stored) = store.load_adc_json(name)? {
            if refresh_token(&stored) == Some(live_token) {
                return Ok(Some(name.clone()));
            }
        }
    }
    Ok(None)
}

fn refresh_token(adc: &Value) -> Option<&str> {
    adc_user_creds(adc)?.get("refresh_token")?.as_str()
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
