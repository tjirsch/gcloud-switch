use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SyncMode {
    #[default]
    Strict,
    Add,
    Off,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub user_account: String,
    /// `core/project` of the gcloud configuration. Empty = none.
    pub user_project: String,
    pub adc_account: String,
    /// `quota_project_id` of the ADC file. Empty = none.
    pub adc_quota_project: String,
    /// Unix timestamp (seconds) when this profile was last modified. Used for sync merge (newer wins). None = treat as old.
    #[serde(default)]
    pub updated_at: Option<i64>,
}

impl Profile {
    /// Set updated_at to current time (for sync merge).
    pub fn touch(&mut self) {
        self.updated_at = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfilesFile {
    #[serde(default)]
    pub active_profile: Option<String>,
    #[serde(default)]
    pub sync_mode: SyncMode,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}
