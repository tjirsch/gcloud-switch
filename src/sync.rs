//! Sync profile metadata via a user-supplied Git remote.
//! No credentials are synced; only profile names and account/project identifiers.
//! Uses the system `git` CLI so the user's git auth (SSH or credential helper) is used.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::profile::{Profile, ProfilesFile};
use crate::store::Store;

fn run_git(repo_path: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .current_dir(repo_path)
        .args(args)
        .output()
        .context("Failed to run git")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("git failed: {}", stderr);
    }
    Ok(out.stdout)
}

/// Ensure sync repo is cloned. If it doesn't exist, clone the remote (or init + remote if empty).
pub fn ensure_cloned(store: &Store, remote_url: &str, branch: &str) -> Result<()> {
    let repo_path = store.sync_repo_path();
    if repo_path.join(".git").exists() {
        return Ok(());
    }
    let parent = repo_path.parent().context("repo path has no parent")?;
    fs::create_dir_all(parent)?;
    let path_str = repo_path.as_os_str().to_str().context("repo path")?;
    let s = Command::new("git")
        .current_dir(parent)
        .args(["clone", "--branch", branch, remote_url, path_str])
        .status();
    if s.as_ref().map(|st| !st.success()).unwrap_or(true) {
        let s2 = Command::new("git")
            .current_dir(parent)
            .args(["clone", remote_url, path_str])
            .status();
        if s2.as_ref().map(|st| !st.success()).unwrap_or(true) {
            // Empty remote: init and add remote; first push will create the branch
            fs::create_dir_all(&repo_path)?;
            Command::new("git").current_dir(&repo_path).args(["init"]).status().context("git init")?;
            Command::new("git")
                .current_dir(&repo_path)
                .args(["remote", "add", "origin", remote_url])
                .status()
                .context("git remote add")?;
        }
    }
    Ok(())
}

/// Push current sync files to the remote. Clones if needed.
pub fn sync_push(store: &Store, remote_url: &str, branch: &str, sync_files: &[String]) -> Result<()> {
    ensure_cloned(store, remote_url, branch)?;
    let repo_path = store.sync_repo_path();

    for filename in sync_files {
        let local_path = store.sync_file_path(filename);
        let repo_file_path = repo_path.join(filename);
        
        if filename == "profiles.toml" {
            let data = store.load_profiles()?;
            let content = toml::to_string_pretty(&data).context("Failed to serialize profiles.toml")?;
            fs::write(&repo_file_path, content)?;
        } else {
            if local_path.exists() {
                fs::copy(&local_path, &repo_file_path)?;
            }
        }
        
        run_git(&repo_path, &["add", filename])?;
    }
    
    if run_git(&repo_path, &["commit", "-m", "gcloud-switch sync"]).is_err() {
        // Nothing to commit (working tree clean) is ok
    }
    run_git(
        &repo_path,
        &["push", "-u", "origin", branch],
    )?;
    Ok(())
}

/// Fetch and merge: get remote sync files, merge profiles.toml by timestamp (newer wins), resolve conflicts by prompting.
pub fn sync_pull(store: &Store, remote_url: &str, branch: &str, sync_files: &[String]) -> Result<()> {
    ensure_cloned(store, remote_url, branch)?;
    let repo_path = store.sync_repo_path();

    run_git(&repo_path, &["fetch", "origin", branch])?;

    let remote_ref = format!("origin/{}", branch);
    
    // First, checkout the remote branch to get all files
    run_git(&repo_path, &["checkout", "-B", branch, remote_ref.as_str()])?;
    
    for filename in sync_files {
        let repo_file_path = repo_path.join(filename);
        
        if filename == "profiles.toml" {
            let remote_content = if repo_file_path.exists() {
                fs::read_to_string(&repo_file_path)?
            } else {
                String::new()
            };
            
            let local = store.load_profiles()?;
            let remote_profiles: ProfilesFile = toml::from_str(&remote_content)
                .unwrap_or_else(|_| ProfilesFile::default());

            let merged = merge_profiles(&local, &remote_profiles)?;
            store.save_profiles(&merged)?;
            
            // Update sync repo with merged version
            let content = toml::to_string_pretty(&merged)?;
            fs::write(&repo_file_path, content)?;
            run_git(&repo_path, &["add", filename])?;
        } else {
            // For non-profiles.toml files, just copy remote version to local
            if repo_file_path.exists() {
                let local_path = store.sync_file_path(filename);
                if let Some(parent) = local_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&repo_file_path, &local_path)?;
            }
        }
    }
    
    // Commit merged state
    if run_git(&repo_path, &["commit", "-m", "gcloud-switch sync merge"]).is_err() {
        // No change after merge is ok
    }

    Ok(())
}

/// Merge local and remote: newer wins per profile; new remote profiles inserted; on conflict prompt which to keep.
fn merge_profiles(local: &ProfilesFile, remote: &ProfilesFile) -> Result<ProfilesFile> {
    let mut out = local.clone();
    for (name, remote_prof) in &remote.profiles {
        match out.profiles.get(name) {
            Some(local_prof) => {
                let local_ts = local_prof.updated_at.unwrap_or(0);
                let remote_ts = remote_prof.updated_at.unwrap_or(0);
                if remote_ts > local_ts {
                    out.profiles.insert(name.clone(), remote_prof.clone());
                } else if remote_ts == local_ts && remote_ts != 0 && *local_prof != *remote_prof {
                    let choice = prompt_which_to_keep(name, local_prof, remote_prof)?;
                    match choice {
                        MergeChoice::Local => {}
                        MergeChoice::Remote => {
                            out.profiles.insert(name.clone(), remote_prof.clone());
                        }
                    }
                }
            }
            None => {
                out.profiles.insert(name.clone(), remote_prof.clone());
            }
        }
    }
    Ok(out)
}

enum MergeChoice {
    Local,
    Remote,
}

fn prompt_which_to_keep(name: &str, local: &Profile, remote: &Profile) -> Result<MergeChoice> {
    fn dash(s: &str) -> &str {
        if s.is_empty() { "-" } else { s }
    }
    let describe = |p: &Profile| {
        format!(
            "{} / {} (adc: {} / {})",
            p.user_account,
            dash(&p.user_project),
            p.adc_account,
            dash(&p.adc_quota_project)
        )
    };
    eprintln!("Profile '{}' changed on both sides.", name);
    eprintln!("  Local:  {}", describe(local));
    eprintln!("  Remote: {}", describe(remote));
    eprint!("Keep (L)ocal or (R)emote? [L/r]: ");
    io::stderr().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    let choice = buf.trim().to_lowercase();
    if choice.starts_with('r') {
        Ok(MergeChoice::Remote)
    } else {
        Ok(MergeChoice::Local)
    }
}
