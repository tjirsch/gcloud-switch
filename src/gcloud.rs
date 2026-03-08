use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::store::Store;

fn gcloud_config_dir() -> Result<PathBuf> {
    // gcloud always uses ~/.config/gcloud on all platforms, ignoring XDG/macOS conventions,
    // unless CLOUDSDK_CONFIG is set.
    if let Ok(custom) = std::env::var("CLOUDSDK_CONFIG") {
        return Ok(PathBuf::from(custom));
    }
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".config").join("gcloud"))
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

/// Create a gcloud configuration without activating it.
pub fn create_configuration(name: &str, account: &str, project: &str) -> Result<()> {
    // Create config — ignore failure if it already exists
    let status = Command::new("gcloud")
        .args(["config", "configurations", "create", name, "--no-activate"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("Failed to run gcloud (is it installed and in PATH?)")?;

    // Verify the configuration file actually exists (covers both fresh-create and already-exists)
    let config_file = configurations_dir()?.join(format!("config_{}", name));
    if !config_file.exists() && !status.success() {
        anyhow::bail!(
            "Failed to create gcloud configuration '{}'. Check that gcloud CLI is working.",
            name
        );
    }

    if !account.is_empty() {
        let status = Command::new("gcloud")
            .args(["config", "set", "account", account, &format!("--configuration={}", name)])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .context("Failed to run gcloud config set account")?;
        if !status.success() {
            anyhow::bail!("Failed to set account on gcloud configuration '{}'", name);
        }
    }

    if !project.is_empty() {
        let status = Command::new("gcloud")
            .args(["config", "set", "project", project, &format!("--configuration={}", name)])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .context("Failed to run gcloud config set project")?;
        if !status.success() {
            anyhow::bail!("Failed to set project on gcloud configuration '{}'", name);
        }
    }

    Ok(())
}

/// Delete a gcloud configuration.
pub fn delete_configuration(name: &str) -> Result<()> {
    let _ = Command::new("gcloud")
        .args(["config", "configurations", "delete", name, "--quiet"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    Ok(())
}

fn configurations_dir() -> Result<PathBuf> {
    let dir = gcloud_config_dir()?.join("configurations");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Activate a profile's user credentials via gcloud CLI.
pub fn activate_user(profile_name: &str, account: &str, project: &str) -> Result<()> {
    // Create configuration if it doesn't exist (ignore error if already exists)
    let _ = Command::new("gcloud")
        .args(["config", "configurations", "create", profile_name, "--no-activate"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    // Activate the configuration
    let output = Command::new("gcloud")
        .args(["config", "configurations", "activate", profile_name])
        .stdout(std::process::Stdio::null())
        .output()
        .context("Failed to activate gcloud configuration")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "gcloud config configurations activate failed for '{}': {}",
            profile_name,
            stderr.trim()
        );
    }

    // Set account and project on the active configuration
    if !account.is_empty() {
        let output = Command::new("gcloud")
            .args(["config", "set", "account", account])
            .stdout(std::process::Stdio::null())
            .output()
            .context("Failed to set gcloud account")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "gcloud config set account failed for '{}': {}",
                account,
                stderr.trim()
            );
        }
    }

    if !project.is_empty() {
        let output = Command::new("gcloud")
            .args(["config", "set", "project", project])
            .stdout(std::process::Stdio::null())
            .output()
            .context("Failed to set gcloud project")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "gcloud config set project failed for '{}': {}",
                project,
                stderr.trim()
            );
        }
    }

    Ok(())
}

/// Activate a profile's ADC credentials.
/// No gcloud CLI equivalent exists, so this copies the stored ADC JSON directly.
pub fn activate_adc(store: &Store, profile_name: &str) -> Result<()> {
    let src = store.adc_path(profile_name);
    if !src.exists() {
        anyhow::bail!(
            "No ADC credentials stored for profile '{}'. Run re-auth (r) first.",
            profile_name
        );
    }
    let config_dir = gcloud_config_dir()?;
    let dest = config_dir.join("application_default_credentials.json");
    fs::copy(&src, &dest).with_context(|| {
        format!(
            "Failed to copy ADC from {} to {}",
            src.display(),
            dest.display()
        )
    })?;
    Ok(())
}

/// Activate both user and ADC credentials for a profile.
pub fn activate_both(
    store: &Store,
    profile_name: &str,
    account: &str,
    project: &str,
) -> Result<()> {
    activate_user(profile_name, account, project)?;
    // ADC activation is best-effort if no ADC file exists yet
    if store.has_adc(profile_name) {
        activate_adc(store, profile_name)?;
    }
    Ok(())
}

/// Re-authenticate user credentials via `gcloud auth login`.
pub fn reauth_user(account: &str) -> Result<()> {
    let status = Command::new("gcloud")
        .args(["auth", "login", &format!("--account={}", account)])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .context("Failed to run gcloud auth login")?;
    if !status.success() {
        anyhow::bail!("gcloud auth login failed with status {}", status);
    }
    Ok(())
}

/// Re-authenticate ADC via `gcloud auth application-default login`, then store the result.
pub fn reauth_adc(store: &Store, profile_name: &str, quota_project: &str) -> Result<()> {
    let status = Command::new("gcloud")
        .args([
            "auth",
            "application-default",
            "login",
            "--quiet",
        ])
        .status()
        .context("Failed to run gcloud auth application-default login")?;
    if !status.success() {
        anyhow::bail!(
            "gcloud auth application-default login failed with status {}",
            status
        );
    }

    // Set quota project
    let _ = Command::new("gcloud")
        .args([
            "auth",
            "application-default",
            "set-quota-project",
            quota_project,
        ])
        .status();

    // Copy the newly created ADC to our store
    let config_dir = gcloud_config_dir()?;
    let adc_src = config_dir.join("application_default_credentials.json");
    if adc_src.exists() {
        let content = fs::read_to_string(&adc_src)?;
        let value: serde_json::Value = serde_json::from_str(&content)?;
        store.save_adc_json(profile_name, &value)?;
    }

    Ok(())
}

/// Set the ADC quota project via `gcloud auth application-default set-quota-project`.
pub fn set_adc_quota_project(quota_project: &str) -> Result<()> {
    let status = Command::new("gcloud")
        .args([
            "auth",
            "application-default",
            "set-quota-project",
            quota_project,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("Failed to run gcloud auth application-default set-quota-project")?;
    if !status.success() {
        anyhow::bail!("gcloud auth application-default set-quota-project failed");
    }
    Ok(())
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
pub fn read_gcloud_credentials(account: &str) -> Result<Option<serde_json::Value>> {
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
            let value: serde_json::Value = serde_json::from_str(&blob)
                .context("Failed to parse credentials blob as JSON")?;
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

/// Validate a refresh token by attempting a token exchange.
pub fn validate_token_blocking(credentials: &serde_json::Value) -> Result<bool> {
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

    let client = reqwest::blocking::Client::new();
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

/// Import existing gcloud configurations as profiles.
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
