//! Uploading a capture and remembering the link (`history.json` next to the config).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Result;
use gpui_kit::{App, ClipboardItem, Task};
use serde::{Deserialize, Serialize};
use shotty_share::Uploaded;

use crate::config::{Config, LinkKind};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub path: PathBuf,
    pub uploaded_at: String,
    pub upload: Uploaded,
}

fn history_path() -> PathBuf {
    crate::config::support_dir().join("history.json")
}

pub fn history() -> Vec<Entry> {
    std::fs::read_to_string(history_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save_history(entries: &[Entry]) {
    let path = history_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_string_pretty(entries).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

/// Progress of a running upload in per mille, readable from the UI while it runs.
pub type Progress = Arc<AtomicU32>;

pub fn fraction(p: &Progress) -> f32 {
    p.load(Ordering::Relaxed) as f32 / 1000.
}

/// Uploads `path` in the background. On success the link is on the clipboard and in the history.
pub fn upload(path: PathBuf, cx: &mut App) -> (Task<Result<Uploaded>>, Progress) {
    let config: Config = cx.global::<crate::Shotty>().config.clone();
    let progress: Progress = Arc::new(AtomicU32::new(0));
    let p = progress.clone();
    let file = path.clone();
    let job = cx.background_executor().spawn(async move {
        let client = shotty_share::Client::new(Some(&config.upload.url), config.upload.token_path.as_deref())?;
        let description = file.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        client.upload(&file, &description, &mut |f| p.store((f * 1000.) as u32, Ordering::Relaxed))
    });
    let task = cx.spawn(async move |cx| {
        let uploaded = job.await?;
        cx.update(|cx| {
            let link = match config.upload.copy {
                LinkKind::Page => uploaded.url.clone(),
                LinkKind::Direct => uploaded.direct.clone(),
            };
            if std::env::var_os("SHOTTY_DEMO").is_none() {
                cx.write_to_clipboard(ClipboardItem::new_string(link));
            }
            let mut entries = history();
            entries.push(Entry { path, uploaded_at: chrono::Local::now().to_rfc3339(), upload: uploaded.clone() });
            save_history(&entries);
            crate::refresh_menu(cx);
        });
        Ok(uploaded)
    });
    (task, progress)
}

/// Deletes the newest upload on the server and drops it from the history.
pub fn delete_last(cx: &mut App) -> Task<Result<()>> {
    let config: Config = cx.global::<crate::Shotty>().config.clone();
    cx.background_executor().spawn(async move {
        let mut entries = history();
        let Some(last) = entries.pop() else { return Ok(()) };
        shotty_share::Client::new(Some(&config.upload.url), config.upload.token_path.as_deref())?.delete(&last.upload.id)?;
        save_history(&entries);
        Ok(())
    })
}
