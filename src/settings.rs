use std::{fs, path::Path};

use anyhow::Context;
use serde::{Deserialize, Serialize};

const SETTINGS_VERSION: u16 = 1;
const SETTINGS_FILE: &str = "settings.json";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    #[default]
    Midnight,
    Graphite,
    Aurora,
    Daylight,
}

impl ThemeChoice {
    pub const ALL: [Self; 4] = [Self::Midnight, Self::Graphite, Self::Aurora, Self::Daylight];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Midnight => "Midnight",
            Self::Graphite => "Graphite",
            Self::Aurora => "Aurora",
            Self::Daylight => "Daylight",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccentChoice {
    #[default]
    Violet,
    Blue,
    Mint,
    Coral,
}

impl AccentChoice {
    pub const ALL: [Self; 4] = [Self::Violet, Self::Blue, Self::Mint, Self::Coral];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Violet => "Violet",
            Self::Blue => "Blue",
            Self::Mint => "Mint",
            Self::Coral => "Coral",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageDensity {
    Compact,
    #[default]
    Cozy,
}

impl MessageDensity {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Cozy => "Cozy",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AppSettings {
    pub version: u16,
    pub profile_name: String,
    pub theme: ThemeChoice,
    pub accent: AccentChoice,
    pub density: MessageDensity,
    pub ui_scale: f32,
    pub show_member_list: bool,
    pub show_message_ids: bool,
    pub enter_to_send: bool,
    pub reduced_motion: bool,
    pub show_channel_intro: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            profile_name: String::new(),
            theme: ThemeChoice::Midnight,
            accent: AccentChoice::Violet,
            density: MessageDensity::Cozy,
            ui_scale: 1.0,
            show_member_list: true,
            show_message_ids: false,
            enter_to_send: true,
            reduced_motion: false,
            show_channel_intro: true,
        }
    }
}

impl AppSettings {
    pub fn load(data_dir: &Path) -> anyhow::Result<Self> {
        let path = data_dir.join(SETTINGS_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let mut settings: Self =
            serde_json::from_slice(&bytes).context("decode Opencord settings")?;
        settings.normalize();
        Ok(settings)
    }

    pub fn save(&self, data_dir: &Path) -> anyhow::Result<()> {
        fs::create_dir_all(data_dir).with_context(|| format!("create {}", data_dir.display()))?;
        let path = data_dir.join(SETTINGS_FILE);
        let temporary = path.with_extension("tmp");
        let bytes = serde_json::to_vec_pretty(self).context("encode Opencord settings")?;
        fs::write(&temporary, bytes).with_context(|| format!("write {}", temporary.display()))?;
        if path.exists() {
            fs::copy(&temporary, &path).with_context(|| format!("replace {}", path.display()))?;
            fs::remove_file(&temporary)
                .with_context(|| format!("remove {}", temporary.display()))?;
        } else {
            fs::rename(&temporary, &path).with_context(|| format!("replace {}", path.display()))?;
        }
        Ok(())
    }

    pub fn normalize(&mut self) {
        self.version = SETTINGS_VERSION;
        self.profile_name = self.profile_name.trim().chars().take(48).collect();
        self.ui_scale = self.ui_scale.clamp(0.85, 1.20);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_normalize() {
        let directory = tempfile::tempdir().unwrap();
        let settings = AppSettings {
            theme: ThemeChoice::Aurora,
            accent: AccentChoice::Mint,
            profile_name: "  Cody  ".into(),
            ui_scale: 9.0,
            ..Default::default()
        };
        settings.save(directory.path()).unwrap();
        let loaded = AppSettings::load(directory.path()).unwrap();
        assert_eq!(loaded.theme, ThemeChoice::Aurora);
        assert_eq!(loaded.accent, AccentChoice::Mint);
        assert_eq!(loaded.profile_name, "Cody");
        assert_eq!(loaded.ui_scale, 1.2);

        let mut updated = loaded;
        updated.theme = ThemeChoice::Daylight;
        updated.save(directory.path()).unwrap();
        assert_eq!(
            AppSettings::load(directory.path()).unwrap().theme,
            ThemeChoice::Daylight
        );
    }
}
