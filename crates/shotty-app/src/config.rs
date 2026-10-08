//! `~/Library/Application Support/Shotty/config.toml`; missing keys fall back to the defaults.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Where captures are saved.
    pub save_dir: PathBuf,
    pub hotkeys: HotkeyConfig,
    pub recording: RecordingConfig,
    pub upload: UploadConfig,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
    /// The viewer page (link previews in chats, download button).
    Page,
    /// The file itself (for Markdown/HTML embedding).
    Direct,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UploadConfig {
    pub url: String,
    /// `None`: the artifacts CLI's `~/.config/artifacts/token`.
    pub token_path: Option<PathBuf>,
    pub copy: LinkKind,
}

impl Default for UploadConfig {
    fn default() -> Self {
        Self { url: shotty_share::DEFAULT_URL.into(), token_path: None, copy: LinkKind::Page }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingConfig {
    pub fps: u32,
    pub show_cursor: bool,
    pub show_clicks: bool,
    /// Initial state of the microphone toggle; the toggle remembers the last choice.
    pub microphone: bool,
    pub system_audio: bool,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self { fps: 30, show_cursor: true, show_clicks: true, microphone: false, system_audio: false }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub capture_screen: String,
    pub capture_area: String,
    pub record: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            capture_screen: "shift+super+Digit3".into(),
            capture_area: "shift+super+Digit4".into(),
            record: "shift+super+Digit5".into(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        let pictures = dirs::picture_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Pictures"));
        Self { save_dir: pictures.join("Shotty"), hotkeys: HotkeyConfig::default(), recording: RecordingConfig::default(), upload: UploadConfig::default() }
    }
}

/// `~/Library/Application Support/Shotty`, or `$SHOTTY_SUPPORT` (tests and demos).
pub fn support_dir() -> PathBuf {
    std::env::var_os("SHOTTY_SUPPORT").map(PathBuf::from).unwrap_or_else(|| dirs::data_dir().unwrap_or_default().join("Shotty"))
}

impl Config {
    pub fn path() -> PathBuf {
        std::env::var_os("SHOTTY_CONFIG").map(PathBuf::from).unwrap_or_else(|| support_dir().join("config.toml"))
    }

    /// Loads the config, writing the defaults on first start so they are easy to edit.
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                eprintln!("shotty: {} is invalid, using defaults: {e}", path.display());
                Self::default()
            }),
            Err(_) => {
                let config = Self::default();
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&path, toml::to_string_pretty(&config).unwrap_or_default());
                config
            }
        }
    }
}
