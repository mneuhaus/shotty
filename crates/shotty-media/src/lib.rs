//! Post-processing of screen recordings (probe, thumbnails, compress/trim, GIF) by shelling out to
//! ffmpeg/ffprobe.
//!
//! A subprocess instead of libav bindings: ffmpeg comes from Homebrew on the target Macs, its CLI is
//! the stable interface, and a crashing encoder can't take the GUI down with it. Arguments always go
//! through `Command::args` (never a shell), so names like "Shotty 2026-10-08 at 13.10.00.mp4" or
//! umlauts need no quoting.
//!
//! The long operations ([`compress`], [`gif`]) run ffmpeg with `-progress pipe:1`, map its output
//! clock onto the (trimmed) clip length and poll a cancel flag. They write to a hidden temp file
//! next to the target and rename it into place only on success, so `output` is always either the
//! previous file or the complete new one, never half an encode. That also makes in-place
//! processing (`output == input`) safe.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Duration;
use std::{env, fmt, fs, thread};

use anyhow::{Context, Result, anyhow, bail, ensure};
use image::{ImageFormat, RgbaImage};

// ---------------------------------------------------------------------------------------------
// Locating the tools

/// `$SHOTTY_FFMPEG`, then Homebrew (Apple Silicon, then Intel), then `$PATH`. The fixed Homebrew
/// paths come before `$PATH` because an app launched from Finder only gets `/usr/bin:/bin:…`.
pub fn ffmpeg_path() -> Option<PathBuf> {
    env_tool("SHOTTY_FFMPEG").or_else(|| search_tool("ffmpeg"))
}

/// `$SHOTTY_FFPROBE`, then the ffprobe next to [`ffmpeg_path`] (so both come from the same build),
/// then the same search as for ffmpeg.
pub fn ffprobe_path() -> Option<PathBuf> {
    env_tool("SHOTTY_FFPROBE")
        .or_else(|| ffmpeg_path().map(|f| f.with_file_name("ffprobe")).filter(|p| is_executable(p)))
        .or_else(|| search_tool("ffprobe"))
}

fn env_tool(var: &str) -> Option<PathBuf> {
    env::var_os(var).map(PathBuf::from).filter(|p| is_executable(p))
}

fn search_tool(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH").map(|p| env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default();
    ["/opt/homebrew/bin", "/usr/local/bin"]
        .map(PathBuf::from)
        .into_iter()
        .chain(path)
        .map(|dir| dir.join(name))
        .find(|p| is_executable(p))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    path.is_file()
}

fn ffmpeg() -> Result<PathBuf> {
    ffmpeg_path().ok_or_else(|| anyhow!("ffmpeg not found (install it with `brew install ffmpeg` or set SHOTTY_FFMPEG)"))
}

fn ffprobe() -> Result<PathBuf> {
    ffprobe_path().ok_or_else(|| anyhow!("ffprobe not found (install it with `brew install ffmpeg` or set SHOTTY_FFPROBE)"))
}

// ---------------------------------------------------------------------------------------------
// Probe

#[derive(Clone, Debug, PartialEq)]
pub struct Probe {
    /// Seconds, container duration (what players show); 0 if ffprobe can't tell.
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    /// Average frame rate. Screen recordings can be variable-rate (ScreenCaptureKit only delivers
    /// a frame when the screen changes), so this can be well below the rate recording started with.
    pub fps: f64,
    pub has_audio: bool,
    pub size_bytes: u64,
}

pub fn probe(path: &Path) -> Result<Probe> {
    Ok(probe_full(path)?.probe)
}

/// [`Probe`] plus what only the encoders need.
struct Info {
    probe: Probe,
    /// Highest rate frames ever arrive at (max of average and `r_frame_rate`); an fps cap only
    /// applies below this, otherwise it would duplicate frames instead of capping.
    max_fps: f64,
    audio_channels: u32,
}

fn probe_full(path: &Path) -> Result<Info> {
    let size_bytes = fs::metadata(path).with_context(|| format!("reading {}", path.display()))?.len();
    let out = Command::new(ffprobe()?)
        .args(["-v", "error", "-show_entries"])
        .arg("format=duration:stream=codec_type,width,height,avg_frame_rate,r_frame_rate,channels,duration:stream_disposition=attached_pic")
        .arg(file_arg(path))
        .stdin(Stdio::null())
        .output()
        .context("running ffprobe")?;
    if !out.status.success() {
        bail!("ffprobe failed on {} ({}): {}", path.display(), out.status, stderr_tail(&out.stderr));
    }
    parse_probe(&String::from_utf8_lossy(&out.stdout), size_bytes).with_context(|| format!("probing {}", path.display()))
}

/// Parses ffprobe's default writer: `[STREAM]`/`[FORMAT]` sections of `key=value` lines. Simple
/// enough that it isn't worth a JSON dependency.
fn parse_probe(text: &str, size_bytes: u64) -> Result<Info> {
    let mut sections: Vec<(&str, Vec<(&str, &str)>)> = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if !name.starts_with('/') {
                sections.push((name, Vec::new()));
            }
        } else if let (Some(kv), Some((_, section))) = (line.split_once('='), sections.last_mut()) {
            section.push(kv);
        }
    }
    fn get<'a>(section: &[(&str, &'a str)], key: &str) -> Option<&'a str> {
        section.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }
    fn num(section: &[(&str, &str)], key: &str) -> Option<f64> {
        get(section, key)?.parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.)
    }
    let streams = || sections.iter().filter(|(name, _)| *name == "STREAM").map(|(_, s)| s.as_slice());

    // Cover art in an MP4 is a one-frame "video" stream; never pick that as the picture.
    let video = streams()
        .find(|s| get(s, "codec_type") == Some("video") && get(s, "DISPOSITION:attached_pic") != Some("1"))
        .context("no video stream")?;
    let audio = streams().find(|s| get(s, "codec_type") == Some("audio"));
    let format = sections.iter().find(|(name, _)| *name == "FORMAT").map(|(_, s)| s.as_slice());
    let dim = |key| {
        get(video, key)
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| *v > 0)
            .with_context(|| format!("video stream has no {key}"))
    };
    let rate = |key| get(video, key).and_then(parse_rate);
    let fps = rate("avg_frame_rate").or(rate("r_frame_rate")).unwrap_or(0.);
    Ok(Info {
        probe: Probe {
            duration: format.and_then(|f| num(f, "duration")).or(num(video, "duration")).unwrap_or(0.),
            width: dim("width")?,
            height: dim("height")?,
            fps,
            has_audio: audio.is_some(),
            size_bytes,
        },
        max_fps: fps.max(rate("r_frame_rate").unwrap_or(0.)),
        audio_channels: audio.and_then(|a| get(a, "channels")?.parse().ok()).unwrap_or(2),
    })
}

/// "30000/1001" → 29.97; "0/0" (unknown) → None.
fn parse_rate(v: &str) -> Option<f64> {
    let (n, d) = v.split_once('/').unwrap_or((v, "1"));
    let (n, d) = (n.parse::<f64>().ok()?, d.parse::<f64>().ok()?);
    (n > 0. && d > 0.).then(|| n / d)
}

// ---------------------------------------------------------------------------------------------
// Single frames

/// One frame at `at` seconds, scaled down to at most `max_width` (keeps aspect, never upscales;
/// 0 = full size), as RGBA.
///
/// Input seeking (`-ss` before `-i`) jumps to the previous keyframe and decodes forward, so it is
/// exact but costs up to one GOP of decoding. Positions at or past the last frame (e.g. a trim
/// handle at the very end) yield the last frame instead of an error.
pub fn frame(path: &Path, at: f64, max_width: u32) -> Result<RgbaImage> {
    let ffmpeg = ffmpeg()?;
    let at = if at.is_finite() { at.max(0.) } else { 0. };
    // PNG through a pipe: self-describing (no need to know the scaled size up front), and level 0
    // skips deflate work on both ends.
    let mut args = argv(["-nostdin", "-v", "error", "-ss", &secs(at), "-i"]);
    args.push(file_arg(path));
    args.extend(os(["-map", "0:v:0", "-frames:v", "1", "-vf", &thumb_filter(max_width)]));
    args.extend(os(["-c:v", "png", "-compression_level", "0", "-f", "image2pipe", "pipe:1"]));
    let out = Command::new(&ffmpeg).args(&args).stdin(Stdio::null()).output().context("running ffmpeg")?;
    if !out.status.success() {
        bail!("ffmpeg could not read a frame at {at:.3}s from {} ({}): {}", path.display(), out.status, stderr_tail(&out.stderr));
    }
    let png = if out.stdout.is_empty() { last_frame_png(&ffmpeg, path, max_width)? } else { out.stdout };
    Ok(image::load_from_memory_with_format(&png, ImageFormat::Png).context("decoding ffmpeg's frame")?.to_rgba8())
}

/// Nothing decodes at or after the last video frame (the end of the clip, or a mic track that
/// outlasts the picture), so take the last frame instead. `-sseof -1` without accurate seeking
/// starts at the keyframe before the final second of the *container* and decodes everything from
/// there, so it still reaches the last frame when the video stopped long before the audio; the
/// image2 muxer keeps overwriting one file (`-update 1`), which leaves just that frame without
/// holding a GOP of full-size frames in memory.
fn last_frame_png(ffmpeg: &Path, path: &Path, max_width: u32) -> Result<Vec<u8>> {
    let scratch = Scratch::in_temp_dir("png");
    let mut args = argv(["-nostdin", "-v", "error", "-y", "-noaccurate_seek", "-sseof", "-1", "-i"]);
    args.push(file_arg(path));
    args.extend(os(["-map", "0:v:0", "-fps_mode", "passthrough", "-vf", &thumb_filter(max_width)]));
    args.extend(os(["-c:v", "png", "-compression_level", "0", "-update", "1", "-f", "image2"]));
    args.push(file_arg(&scratch.path));
    let out = Command::new(ffmpeg).args(&args).stdin(Stdio::null()).output().context("running ffmpeg")?;
    if !out.status.success() {
        bail!("ffmpeg could not read the last frame of {} ({}): {}", path.display(), out.status, stderr_tail(&out.stderr));
    }
    fs::read(&scratch.path).with_context(|| format!("{} has no video frames", path.display()))
}

fn thumb_filter(max_width: u32) -> String {
    // `min()` keeps small clips at their size; the escaped comma stops the graph parser from
    // splitting the expression into two filters; h=-1 follows the aspect.
    let scale = if max_width == 0 { String::new() } else { format!("scale=min(iw\\,{max_width}):-1:flags=lanczos,") };
    format!("{scale}format=rgba")
}

// ---------------------------------------------------------------------------------------------
// Options

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trim {
    /// Seconds from the start of the clip.
    pub start: f64,
    /// Seconds from the start of the clip; `end <= duration` (larger values are clamped).
    pub end: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// libx264, plays everywhere (browsers, WhatsApp, Slack, old phones).
    H264,
    /// libx265 tagged `hvc1`: 20-50 % smaller than H.264 at the same quality setting, ~2.5× slower
    /// to encode, and only Apple platforms and recent Chrome/Edge play it (Firefox on macOS doesn't).
    Hevc,
}

impl Codec {
    /// The CRF `quality` (0..=100) maps to. Linear in CRF, so every ~25 points roughly doubles the
    /// size (6 CRF ≈ 2× bitrate):
    ///
    /// | quality | H.264 (x264) | HEVC (x265) |
    /// |--------:|-------------:|------------:|
    /// |       0 |           38 |          43 |
    /// | 60 (default) |    23.6 |        28.6 |
    /// |     100 |           14 |          19 |
    ///
    /// 100 is visually lossless rather than CRF 0, which would be absurdly large for no visible
    /// gain; 0 is still legible on Retina captures. The default sits just above x264's own default
    /// (23). HEVC is offset by +5, the usual x264↔x265 equivalence (x265's default 28 ≈ x264's
    /// 23). With the encoder settings in [`compress`] that held up visually on 2400×1500 scrolling
    /// code (text equally crisp, slightly more smear right around moving edges), though x265 scores
    /// ~2-3 dB lower PSNR, partly because its psy tuning trades PSNR for perceived detail.
    pub fn crf(self, quality: u8) -> f32 {
        let h264 = 38. - 0.24 * f32::from(quality.min(100));
        match self {
            Codec::H264 => h264,
            Codec::Hevc => h264 + 5.,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompressOptions {
    pub codec: Codec,
    /// 0..=100, higher = better/larger; see [`Codec::crf`] for the mapping. Default 60.
    pub quality: u8,
    /// Downscale so width <= max_width (even dimensions, lanczos). None = keep.
    pub max_width: Option<u32>,
    /// Cap frame rate. None = keep.
    pub fps: Option<u32>,
    pub keep_audio: bool,
    pub trim: Option<Trim>,
}

impl Default for CompressOptions {
    fn default() -> Self {
        Self { codec: Codec::H264, quality: 60, max_width: None, fps: None, keep_audio: true, trim: None }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GifOptions {
    /// Clamped to 1..=50 and to the source rate. GIF delays are whole centiseconds and browsers
    /// slow anything under 2 cs down to 10 cs, so 50 fps is the real ceiling.
    pub fps: u32,
    /// Downscale to this width (lanczos, keeps aspect, never upscales). None = keep.
    pub width: Option<u32>,
    pub trim: Option<Trim>,
    pub loop_forever: bool,
}

impl Default for GifOptions {
    fn default() -> Self {
        Self { fps: 15, width: Some(960), trim: None, loop_forever: true }
    }
}

// ---------------------------------------------------------------------------------------------
// Cancellation

/// Returned (inside the `anyhow::Error`) when the cancel flag stopped an operation; check with
/// [`is_cancelled`] or `err.is::<Cancelled>()`.
#[derive(Debug)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Whether `err` means "the user cancelled" (show nothing) rather than a failure (show it).
pub fn is_cancelled(err: &anyhow::Error) -> bool {
    err.is::<Cancelled>()
}

// ---------------------------------------------------------------------------------------------
// Compress

/// Re-encodes `input` to an MP4 that plays everywhere: yuv420p, even dimensions, `+faststart`
/// (index up front, so browsers and chat apps can start playback before the download finishes),
/// AAC audio. Optionally trims, downscales and caps the frame rate in the same pass.
///
/// Encoder choices, measured on 2400×1500 clips (scrolling code + cursor, and full-motion
/// testsrc2):
/// - x264 `me=umh:merange=32`: the big one. Current x264 presets up to `slow` use a hex search
///   with range 16 (UMH only from `slower`), which never finds a Retina scroll (20+ px per frame),
///   so scrolling text was re-coded from scratch: 4.2× larger. UMH over ±32 finds it, at no cost on screen content
///   and ~1.6× time on full motion; ranges 48/64 bought 3-5 % for 30-90 % more time.
/// - x264 `-preset medium`, not slow: with the search fixed, slow was only ~4 % more efficient
///   for 1.5-2× the time, and its doubled refs (under `animation`) push 2400×1500 to level 5.1;
///   medium stays at 5.0, which more hardware decoders accept. x265 `medium` (its slow was
///   1.4-2.6× slower for ≤10 %); its default search range is 57, so it has no scroll problem.
/// - `-tune animation` (both): screen content is flat areas and hard edges, like cartoons. It
///   lowers psy-rd/AQ (which spend bits faking film-grain texture that UIs don't have) and adds
///   B-frames/refs (static screens repeat). At equal size it measured +0.4-0.6 dB PSNR over no
///   tune; `stillimage` was 0.5 dB worse.
/// - CRF (constant quality), not a bitrate: a static screen then costs almost nothing and
///   scrolling gets what it needs.
pub fn compress(
    input: &Path,
    output: &Path,
    opts: &CompressOptions,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<()> {
    check_cancel(cancel)?;
    let ffmpeg = ffmpeg()?;
    let info = probe_full(input)?;
    let trim = clamp_trim(opts.trim, info.probe.duration)?;
    let length = trim.map_or(info.probe.duration, |t| t.end - t.start);
    let audio = opts.keep_audio && info.probe.has_audio;
    let scratch = Scratch::next_to(output)?;

    let mut args = run_args(input, trim);
    args.extend(os(["-map", "0:v:0"]));
    if audio {
        args.extend(os(["-map", "0:a:0"]));
    }
    args.extend(os(["-vf", &video_filter(&info, opts)]));
    let crf = format!("{:.1}", opts.codec.crf(opts.quality));
    match opts.codec {
        Codec::H264 => args.extend(os([
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-tune",
            "animation",
            "-crf",
            &crf,
            "-x264-params",
            "me=umh:merange=32",
        ])),
        // `hvc1` (parameter sets in the sample entry) instead of ffmpeg's default `hev1`: Apple's
        // players refuse `hev1`. x265 logs to stderr on its own; keep that to real errors.
        Codec::Hevc => args.extend(os([
            "-c:v",
            "libx265",
            "-preset",
            "medium",
            "-tune",
            "animation",
            "-crf",
            &crf,
            "-tag:v",
            "hvc1",
            "-x265-params",
            "log-level=error",
        ])),
    }
    if audio {
        // Mic tracks are mono speech, for which 96k AAC is plenty; stereo (system audio) gets 128k.
        let bitrate = if info.audio_channels == 1 { "96k" } else { "128k" };
        args.extend(os(["-c:a", "aac", "-b:a", bitrate]));
    } else {
        args.push("-an".into());
    }
    args.extend(os(["-movflags", "+faststart", "-f", "mp4"]));
    args.push(file_arg(&scratch.path));

    run(
        &ffmpeg,
        &args,
        &|tick| match *tick {
            Tick::Out { secs: Some(s) } if length > 0. => Some((s / length) as f32),
            _ => None,
        },
        progress,
        cancel,
    )?;
    scratch.persist(output)?;
    progress(1.);
    Ok(())
}

/// fps cap, then (down)scale or crop to even, then yuv420p, in that order so the scaler only sees
/// the frames that survive the cap.
fn video_filter(info: &Info, opts: &CompressOptions) -> String {
    let (w, h) = (info.probe.width, info.probe.height);
    let mut filters = Vec::new();
    if let Some(cap) = opts.fps.filter(|&cap| cap > 0 && f64::from(cap) < info.max_fps - 0.01) {
        filters.push(format!("fps={cap}"));
    }
    match opts.max_width.filter(|&max| max < w) {
        Some(max) => {
            // Explicit even height instead of `-2`, so the output size is known (and tested) here.
            let tw = (max & !1).max(2);
            let th = ((f64::from(h) * f64::from(tw) / f64::from(w) / 2.).round() as u32 * 2).max(2);
            filters.push(format!("scale={tw}:{th}:flags=lanczos"));
        }
        // 4:2:0 needs even dimensions. Crop the odd pixel instead of rescaling: a 1px rescale
        // would resample (blur) every pixel of text that is otherwise pixel-sharp.
        None if w % 2 == 1 || h % 2 == 1 => filters.push(format!("crop={}:{}:0:0", (w & !1).max(2), (h & !1).max(2))),
        None => {}
    }
    filters.push("format=yuv420p".into());
    filters.join(",")
}

// ---------------------------------------------------------------------------------------------
// GIF

/// Converts `input` to an animated GIF in one ffmpeg run with the classic two-pass palette:
///
/// - `palettegen=stats_mode=diff`: the histogram counts the first frame fully and afterwards only
///   pixels that changed, so the 256 colours favour what moves (cursor, scrolled text) over the
///   static desktop, which is already represented by the first frame.
/// - `paletteuse=dither=bayer:bayer_scale=5`: ordered dithering is a fixed pattern, so unchanged
///   pixels stay identical from frame to frame; error diffusion (the default) shimmers everywhere
///   as soon as anything changes and defeats the GIF encoder's inter-frame transparency. Scale 5
///   is the faintest bayer pattern; screen content is mostly flat colours that need little.
/// - `diff_mode=rectangle`: only the changed rectangle is re-dithered, again keeping static areas
///   bit-identical so the encoder can skip them.
///
/// `split` makes ffmpeg hold every (scaled) frame in memory until the palette exists after the
/// last one: roughly width × height × 4 bytes × frames, ~350 MB for 10 s at 960×600 and 15 fps.
/// GIFs are short by nature, but an hour-long one would not be.
pub fn gif(input: &Path, output: &Path, opts: &GifOptions, progress: &mut dyn FnMut(f32), cancel: &AtomicBool) -> Result<()> {
    check_cancel(cancel)?;
    let ffmpeg = ffmpeg()?;
    let info = probe_full(input)?;
    let trim = clamp_trim(opts.trim, info.probe.duration)?;
    let length = trim.map_or(info.probe.duration, |t| t.end - t.start);
    let mut fps = opts.fps.clamp(1, 50);
    if info.max_fps > 0. {
        fps = fps.min(info.max_fps.ceil() as u32).max(1);
    }
    let scale = match opts.width.filter(|&w| w > 0 && w < info.probe.width) {
        Some(w) => format!(",scale={w}:-1:flags=lanczos"),
        None => String::new(),
    };
    let scratch = Scratch::next_to(output)?;

    // Progress has two halves. ffmpeg only starts reporting once the GIF header is written, i.e.
    // after the palette exists, so the analysis pass (decode + scale + histogram, about half the
    // time) would sit at 0 %. The metadata filters tag every analysed frame and print its time to
    // stdout next to the `-progress` blocks (`direct=1` so lines aren't held in a 32 KB buffer;
    // `\\:` because the colon is escaped once for the option and once for the graph).
    let graph = format!(
        "[0:v:0]fps={fps}{scale},metadata=mode=add:key=shotty:value=1,{tap},split[a][b];\
         [a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle[out]",
        tap = r"metadata=mode=print:key=shotty:file=pipe\\:1:direct=1",
    );
    let mut args = run_args(input, trim);
    args.extend(os(["-filter_complex", &graph, "-map", "[out]"]));
    // GIF loop count: 0 = forever, -1 = no NETSCAPE loop block at all, i.e. play once.
    args.extend(os(["-loop", if opts.loop_forever { "0" } else { "-1" }, "-f", "gif"]));
    args.push(file_arg(&scratch.path));

    const ANALYSIS: f64 = 0.5; // measured 45-55 % of the run on both full-motion and screen content
    run(
        &ffmpeg,
        &args,
        &|tick| match *tick {
            _ if length <= 0. => None,
            Tick::Analysed(t) => Some((ANALYSIS * t / length) as f32),
            Tick::Out { secs: Some(s) } => Some((ANALYSIS + (1. - ANALYSIS) * s / length) as f32),
            Tick::Out { secs: None } => None,
        },
        progress,
        cancel,
    )?;
    scratch.persist(output)?;
    progress(1.);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Running ffmpeg

/// Common head of a long run: quiet stderr (only errors, which end up in our error message),
/// machine-readable progress on stdout every 250 ms, and the input with an optional trim.
///
/// `-ss`/`-to` go *before* `-i`: ffmpeg then seeks to the keyframe before `start`, decodes from
/// there and drops everything before `start` (and after `end`) on both video and audio, which is
/// frame-exact when re-encoding and doesn't decode the skipped part. Timestamps restart at 0, so
/// the progress clock runs from 0 to `end - start`.
fn run_args(input: &Path, trim: Option<Trim>) -> Vec<OsString> {
    let mut args = argv(["-nostdin", "-y", "-v", "error", "-nostats", "-progress", "pipe:1", "-stats_period", "0.25"]);
    if let Some(t) = trim {
        args.extend(os(["-ss", &secs(t.start), "-to", &secs(t.end)]));
    }
    args.push("-i".into());
    args.push(file_arg(input));
    args
}

/// What the stdout reader saw.
enum Tick {
    /// A `-progress` block: `out_time` of the output in seconds, if ffmpeg knew it yet.
    Out { secs: Option<f64> },
    /// A frame passed the GIF graph's analysis tap at this (output) time.
    Analysed(f64),
}

const POLL: Duration = Duration::from_millis(40);

/// Runs ffmpeg to completion while forwarding progress (mapped by `fraction`, kept monotonic and
/// below 1.0, since 1.0 means "file is in place") and polling `cancel`.
///
/// stdout and stderr are drained on their own threads so ffmpeg can never block on a full pipe and
/// cancel is noticed within `POLL` even while ffmpeg is silent (probing, faststart rewrite).
fn run(
    ffmpeg: &Path,
    args: &[OsString],
    fraction: &dyn Fn(&Tick) -> Option<f32>,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<()> {
    let mut child = Process(
        Command::new(ffmpeg)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting {}", ffmpeg.display()))?,
    );
    let (stdout, mut stderr) = (child.0.stdout.take().expect("piped"), child.0.stderr.take().expect("piped"));
    let errors = thread::spawn(move || {
        // Bounded: only the tail ends up in the error message anyway.
        let (mut kept, mut buf) = (Vec::new(), [0; 8192]);
        while let Ok(n @ 1..) = stderr.read(&mut buf) {
            kept.extend_from_slice(&buf[..n]);
            if kept.len() > 64 * 1024 {
                kept.drain(..kept.len() - 32 * 1024);
            }
        }
        kept
    });
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || read_ticks(stdout, tx));

    // An immediate 0.0 lets the UI switch to "working", and makes a cancel issued from the very
    // first callback take effect before ffmpeg has produced anything.
    let mut shown = 0f32;
    progress(0.);
    let status: ExitStatus = loop {
        check_cancel(cancel)?; // dropping `child` kills ffmpeg before the caller's Scratch cleans up
        match rx.recv_timeout(POLL) {
            Ok(tick) => {
                // Whole percents are plenty for a progress bar; the GIF tap ticks once per frame.
                if let Some(f) = fraction(&tick).filter(|f| f.is_finite()).map(|f| f.clamp(0., 0.99))
                    && f >= shown + 0.01
                {
                    shown = f;
                    progress(f);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            // stdout closed: ffmpeg is exiting.
            Err(RecvTimeoutError::Disconnected) => match child.0.try_wait().context("waiting for ffmpeg")? {
                Some(status) => break status,
                None => thread::sleep(POLL),
            },
        }
    };
    let errors = errors.join().unwrap_or_default();
    if !status.success() {
        bail!("ffmpeg failed ({status}): {}", stderr_tail(&errors));
    }
    Ok(())
}

/// Parses `-progress` blocks (`key=value` lines ending in `progress=…`) and the GIF tap's
/// `frame:N pts:… pts_time:T` lines. Ends when ffmpeg closes stdout or nobody listens anymore.
fn read_ticks(stdout: ChildStdout, tx: Sender<Tick>) {
    let mut secs = None;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let tick = if line.starts_with("frame:") {
            line.split_once("pts_time:").and_then(|(_, t)| t.trim().parse().ok()).map(Tick::Analysed)
        } else {
            match line.split_once('=') {
                // Both are microseconds (`_ms` is a historic misnomer); N/A or the negative
                // sentinel before the first packet keep the previous value.
                Some(("out_time_us" | "out_time_ms", v)) => {
                    secs = v.parse::<i64>().ok().filter(|us| *us >= 0).map(|us| us as f64 / 1e6).or(secs);
                    None
                }
                Some(("progress", _)) => Some(Tick::Out { secs }),
                _ => None,
            }
        };
        if let Some(tick) = tick
            && tx.send(tick).is_err()
        {
            break;
        }
    }
}

/// Kills ffmpeg if we leave before it finished (cancel, error, a panicking progress callback), so
/// no orphan keeps encoding, and reaps it either way.
struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

/// A file ffmpeg writes that is deleted on drop unless [`Scratch::persist`] moved it into place, so
/// a failed, cancelled or panicked run leaves nothing behind.
struct Scratch {
    path: PathBuf,
    persisted: bool,
}

static SCRATCH_SEQ: AtomicU32 = AtomicU32::new(0);

impl Scratch {
    /// Hidden (`.`-prefixed, so it doesn't flash up in Finder) and in the target's directory, so the
    /// final rename stays on one volume and is atomic.
    fn next_to(target: &Path) -> Result<Self> {
        let name = target.file_name().with_context(|| format!("{} is not a file path", target.display()))?;
        let mut tmp = OsString::from(".");
        tmp.push(name);
        tmp.push(format!(".{}-{}.part", std::process::id(), SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed)));
        Ok(Self { path: target.with_file_name(tmp), persisted: false })
    }

    fn in_temp_dir(ext: &str) -> Self {
        let name = format!("shotty-media-{}-{}.{ext}", std::process::id(), SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed));
        Self { path: env::temp_dir().join(name), persisted: false }
    }

    fn persist(mut self, target: &Path) -> Result<()> {
        fs::rename(&self.path, target).with_context(|| format!("moving the result to {}", target.display()))?;
        self.persisted = true;
        Ok(())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.persisted {
            let _ = fs::remove_file(&self.path);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Small helpers

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) { Err(Cancelled.into()) } else { Ok(()) }
}

/// Validates a trim and clamps it into `0..=duration` (duration 0 = unknown, no upper clamp).
fn clamp_trim(trim: Option<Trim>, duration: f64) -> Result<Option<Trim>> {
    let Some(t) = trim else { return Ok(None) };
    ensure!(t.start.is_finite() && t.end.is_finite(), "invalid trim {:?}", t);
    let start = t.start.max(0.);
    let end = if duration > 0. { t.end.min(duration) } else { t.end };
    ensure!(end - start >= 0.01, "trim {:.3}..{:.3}s leaves nothing of the {:.3}s clip", t.start, t.end, duration);
    Ok(Some(Trim { start, end }))
}

/// A path as ffmpeg should see it. ffmpeg reads `name:rest` as a protocol URL when `name` looks like
/// a scheme, so a relative "clip 13:10.mp4" would fail; anchoring relative paths at `./` keeps
/// every path a plain file (absolute ones start with `/`, which no scheme contains).
fn file_arg(path: &Path) -> OsString {
    if path.is_absolute() { path.into() } else { Path::new(".").join(path).into() }
}

/// Seconds for ffmpeg, in plain decimal (never `1e-7` notation).
fn secs(s: f64) -> String {
    format!("{s:.6}")
}

fn argv<const N: usize>(a: [&str; N]) -> Vec<OsString> {
    a.into_iter().map(OsString::from).collect()
}

fn os<'a>(a: impl IntoIterator<Item = &'a str>) -> impl Iterator<Item = OsString> {
    a.into_iter().map(OsString::from)
}

/// The last few non-empty lines of ffmpeg's stderr: the actual error is at the end, after
/// whatever warnings led up to it.
fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(12)..].join("\n");
    if tail.is_empty() { "(no error output)".into() } else { tail }
}

#[cfg(test)]
mod tests;
