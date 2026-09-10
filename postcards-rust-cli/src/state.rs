//! Persistent state for PostcardsRust automation (tracks last card sent, cooldowns, stats).

use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppState {
    pub last_sent_at: Option<DateTime<Utc>>,
    pub last_photo_name: Option<String>,
    #[serde(default)]
    pub total_cards_sent: u64,
}

impl AppState {
    pub fn state_file_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".postcards_rust")
            .join("state.json")
    }

    pub fn load() -> Self {
        let path = Self::state_file_path();
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(state) = serde_json::from_str::<Self>(&data) {
                return state;
            }
        }
        Self::default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::state_file_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, data)?;
        Ok(())
    }

    pub fn record_sent(&mut self, photo_name: &str) -> anyhow::Result<()> {
        self.last_sent_at = Some(Utc::now());
        self.last_photo_name = Some(photo_name.to_string());
        self.total_cards_sent += 1;
        self.save()
    }

    /// Returns the remaining duration if currently in cooldown, or None if eligible to send.
    pub fn remaining_cooldown(&self, cooldown_days: u32) -> Option<Duration> {
        let last_sent = self.last_sent_at?;
        let cooldown_duration = Duration::days(cooldown_days as i64);
        let eligible_at = last_sent + cooldown_duration;
        let now = Utc::now();
        if now < eligible_at {
            Some(eligible_at - now)
        } else {
            None
        }
    }

    pub fn next_eligible_at(&self, cooldown_days: u32) -> Option<DateTime<Utc>> {
        self.last_sent_at
            .map(|t| t + Duration::days(cooldown_days as i64))
    }
}
