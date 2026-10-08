//! Screen recording straight to an MP4 file via `SCRecordingOutput` (macOS 15+).
//!
//! Shotty's own windows (the frame around the area, the control bar) are excluded from the
//! recording by excluding the whole app from the content filter.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use screencapturekit::prelude::*;
use screencapturekit::recording_output::{
    RecordingCallbacks, SCRecordingOutput, SCRecordingOutputCodec, SCRecordingOutputConfiguration, SCRecordingOutputFileType,
};
use screencapturekit::shareable_content::SCShareableContentInfo;

use crate::{Rect, content};

#[derive(Clone, Debug)]
pub struct RecordOptions {
    pub display_id: u32,
    /// The area to record, in global points; must lie on `display_id`.
    pub area: Rect,
    pub fps: u32,
    pub cursor: bool,
    /// Draws a ring where the mouse clicks.
    pub clicks: bool,
    pub microphone: bool,
    pub system_audio: bool,
    pub path: PathBuf,
}

pub struct Recording {
    stream: SCStream,
    output: SCRecordingOutput,
    path: PathBuf,
    started: Instant,
    failed: Arc<std::sync::Mutex<Option<String>>>,
}

/// H.264 needs even dimensions.
fn even(v: f64) -> u32 {
    ((v.round() as u32) / 2 * 2).max(2)
}

impl Recording {
    pub fn start(opts: &RecordOptions) -> Result<Self> {
        let content = content()?;
        let display = content.displays().into_iter().find(|d| d.display_id() == opts.display_id).context("display is gone")?;
        let own = std::process::id() as i32;
        let apps = content.applications();
        let ours: Vec<&SCRunningApplication> = apps.iter().filter(|a| a.process_id() == own).collect();
        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .with_excluding_applications(&ours, &[])
            .build()
            .map_err(|e| anyhow!("{e}"))?;
        let scale = SCShareableContentInfo::for_filter(&filter).map_or(2.0, |i| f64::from(i.point_pixel_scale()));

        let frame = display.frame();
        let source = CGRect::new(
            opts.area.x - frame.origin.x,
            opts.area.y - frame.origin.y,
            opts.area.width,
            opts.area.height,
        );
        let mut config = SCStreamConfiguration::new()
            .with_source_rect(source)
            .with_width(even(opts.area.width * scale))
            .with_height(even(opts.area.height * scale))
            .with_fps(opts.fps)
            .with_shows_cursor(opts.cursor)
            .with_captures_audio(opts.system_audio)
            .with_excludes_current_process_audio(true)
            .with_queue_depth(6);
        config = config.with_shows_mouse_clicks(opts.clicks).map_err(|e| anyhow!("{e}"))?;
        if opts.microphone {
            config = config.with_captures_microphone(true).map_err(|e| anyhow!("{e}"))?;
        }

        if let Some(dir) = opts.path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let output_config = SCRecordingOutputConfiguration::new()
            .map_err(|e| anyhow!("{e}"))?
            .with_output_url(&opts.path)
            .map_err(|e| anyhow!("{e:?}"))?
            .with_video_codec(SCRecordingOutputCodec::H264)
            .with_output_file_type(SCRecordingOutputFileType::MP4);
        let failed = Arc::new(std::sync::Mutex::new(None));
        let failed_cb = failed.clone();
        let callbacks = RecordingCallbacks::new().on_fail(move |e| {
            log::error!("recording failed: {e}");
            *failed_cb.lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
        });
        let output = SCRecordingOutput::new_with_delegate(&output_config, callbacks).context("recording needs macOS 15")?;
        let stream = SCStream::new(&filter, &config).map_err(|e| anyhow!("{e}"))?;
        stream.add_recording_output(&output).map_err(|e| anyhow!("adding recording output: {e}"))?;
        stream.start_capture().map_err(|e| anyhow!("starting capture: {e}"))?;
        log::info!("recording {}x{} @{} fps to {}", even(opts.area.width * scale), even(opts.area.height * scale), opts.fps, opts.path.display());
        Ok(Self { stream, output, path: opts.path.clone(), started: Instant::now(), failed })
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stops and waits until the file is completely written. Blocks; call off the main thread.
    pub fn stop(self) -> Result<PathBuf> {
        // Removing the only recording output from a stream without frame handlers stops the capture
        // and waits until the file is finalised.
        self.stream.remove_recording_output(&self.output).map_err(|e| anyhow!("finishing recording: {e}"))?;
        let _ = self.stream.stop_capture();
        if let Some(e) = self.failed.lock().unwrap_or_else(|p| p.into_inner()).take() {
            bail!("recording failed: {e}");
        }
        // The file can lag the completion callback by a moment.
        let (tx, rx) = mpsc::channel();
        let path = self.path.clone();
        std::thread::spawn(move || {
            for _ in 0..50 {
                if std::fs::metadata(&path).is_ok_and(|m| m.len() > 0) {
                    let _ = tx.send(());
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        rx.recv_timeout(Duration::from_secs(6)).map_err(|_| anyhow!("no video file was written"))?;
        Ok(self.path)
    }

    /// Stops and throws the file away.
    pub fn cancel(self) {
        let path = self.path.clone();
        let _ = self.stop();
        let _ = std::fs::remove_file(path);
    }
}
