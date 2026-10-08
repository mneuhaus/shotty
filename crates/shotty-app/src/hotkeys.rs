//! System-wide shortcuts via Carbon hotkeys (no Accessibility permission needed).

use std::collections::HashMap;
use std::str::FromStr;

use anyhow::{Context, Result};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::{Command, Event};

pub struct Hotkeys {
    _manager: GlobalHotKeyManager,
    /// Shortcuts that could not be registered, e.g. because another app holds them.
    pub failed: Vec<String>,
}

/// Registers `bindings` ("shift+super+Digit4" style) and sends the matching command on every press.
pub fn register(bindings: &[(Command, String)], tx: async_channel::Sender<Event>) -> Result<Hotkeys> {
    let manager = GlobalHotKeyManager::new().context("hotkey manager")?;
    let mut by_id = HashMap::new();
    let mut failed = Vec::new();
    for (command, spec) in bindings {
        let registered = HotKey::from_str(spec)
            .map_err(anyhow::Error::from)
            .and_then(|hotkey| manager.register(hotkey).map(|()| hotkey).map_err(anyhow::Error::from));
        match registered {
            Ok(hotkey) => {
                by_id.insert(hotkey.id(), *command);
            }
            Err(e) => {
                eprintln!("shotty: hotkey {spec} not available: {e}");
                failed.push(spec.clone());
            }
        }
    }
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state == HotKeyState::Pressed
            && let Some(command) = by_id.get(&event.id)
        {
            let _ = tx.try_send(Event::Command(*command));
        }
    }));
    Ok(Hotkeys { _manager: manager, failed })
}

/// "shift+super+Digit4" → "⇧⌘4" for menus and settings.
pub fn display(spec: &str) -> String {
    let mut out = String::new();
    let mut key = String::new();
    for part in spec.split('+') {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => out.push('⌃'),
            "alt" | "option" => out.push('⌥'),
            "shift" => out.push('⇧'),
            "super" | "cmd" | "command" | "meta" => out.push('⌘'),
            _ => key = part.trim_start_matches("Digit").trim_start_matches("Key").to_string(),
        }
    }
    let order = ['⌃', '⌥', '⇧', '⌘'];
    let mut mods: Vec<char> = out.chars().collect();
    mods.sort_by_key(|c| order.iter().position(|o| o == c));
    mods.into_iter().collect::<String>() + &key
}

#[cfg(test)]
mod tests {
    #[test]
    fn displays_mac_style() {
        assert_eq!(super::display("shift+super+Digit4"), "⇧⌘4");
        assert_eq!(super::display("super+ctrl+KeyR"), "⌃⌘R");
    }
}
