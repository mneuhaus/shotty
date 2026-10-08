//! End-to-end tests against the real ffmpeg, on clips generated with lavfi. Without ffmpeg they
//! print a note and pass, so `cargo test` stays green on machines without Homebrew.

use super::*;

/// A fresh temp dir (removed on drop) holding a generated clip.
struct Fixture {
    dir: PathBuf,
    clip: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Fixture {
    /// 2 s, 640×360, 30 fps H.264 with a mono sine AAC track: the shape of a mic recording.
    fn new(test: &str) -> Option<Self> {
        Self::custom(test, "clip.mp4", "640x360", 2, &["-pix_fmt", "yuv420p"])
    }

    /// `video_secs` of testsrc2 at `size` plus 2 s of sine (so the audio can outlast the picture).
    fn custom(test: &str, name: &str, size: &str, video_secs: u32, video_args: &[&str]) -> Option<Self> {
        let (Some(ffmpeg), Some(_)) = (ffmpeg_path(), ffprobe_path()) else {
            eprintln!("note: ffmpeg/ffprobe not found, skipping {test}");
            return None;
        };
        let dir = env::temp_dir().join(format!("shotty-media test {}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let clip = dir.join(name);
        let out = Command::new(ffmpeg)
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("testsrc2=size={size}:rate=30:duration={video_secs}"))
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-c:v", "libx264", "-preset", "ultrafast"])
            .args(video_args)
            .args(["-c:a", "aac"])
            .arg(&clip)
            .output()
            .unwrap();
        assert!(out.status.success(), "generating fixture: {}", String::from_utf8_lossy(&out.stderr));
        Some(Self { dir, clip })
    }

    fn out(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Everything in the dir except the fixture, to catch leftover `.part` files.
    fn others(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| *n != self.clip.file_name().unwrap().to_string_lossy())
            .collect();
        names.sort();
        names
    }
}

/// One stream field via ffprobe, for things `Probe` deliberately doesn't expose (codec tag, pixel
/// format).
fn stream_field(path: &Path, field: &str) -> String {
    let out = Command::new(ffprobe_path().unwrap())
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", &format!("stream={field}"), "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Runs `f` with a progress recorder and a never-set cancel flag; returns the reported values.
fn recording(f: impl FnOnce(&mut dyn FnMut(f32), &AtomicBool) -> Result<()>) -> Vec<f32> {
    let mut seen = Vec::new();
    f(&mut |p| seen.push(p), &AtomicBool::new(false)).unwrap();
    seen
}

fn assert_progress_sane(seen: &[f32]) {
    assert_eq!(seen.last(), Some(&1.), "must end at 1.0: {seen:?}");
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "must not go backwards: {seen:?}");
    assert!(seen[..seen.len() - 1].iter().all(|p| (0. ..1.).contains(p)), "1.0 only at the end: {seen:?}");
}

#[test]
fn probe_reads_generated_clip() {
    let Some(fx) = Fixture::new("probe") else { return };
    let p = probe(&fx.clip).unwrap();
    assert!((p.duration - 2.).abs() < 0.1, "{p:?}");
    assert_eq!((p.width, p.height), (640, 360));
    assert!((p.fps - 30.).abs() < 0.01, "{p:?}");
    assert!(p.has_audio);
    assert_eq!(p.size_bytes, fs::metadata(&fx.clip).unwrap().len());
}

#[test]
fn probe_rejects_non_video() {
    let Some(fx) = Fixture::new("probe-bad") else { return };
    let bogus = fx.out("bogus.mp4");
    fs::write(&bogus, "not a video").unwrap();
    let err = probe(&bogus).unwrap_err();
    assert!(format!("{err:#}").contains("ffprobe failed"), "{err:#}");
}

#[test]
fn frame_scales_to_max_width() {
    let Some(fx) = Fixture::new("frame") else { return };
    let img = frame(&fx.clip, 1., 320).unwrap();
    assert_eq!(img.dimensions(), (320, 180));
    // Never upscales; 0 means full size.
    assert_eq!(frame(&fx.clip, 0.5, 4000).unwrap().dimensions(), (640, 360));
    assert_eq!(frame(&fx.clip, 0., 0).unwrap().dimensions(), (640, 360));
    // At/after the end there is no frame to seek to; we get the last one instead.
    assert_eq!(frame(&fx.clip, 2., 160).unwrap().dimensions(), (160, 90));
    assert_eq!(frame(&fx.clip, 99., 160).unwrap().dimensions(), (160, 90));
    // The picture is real: testsrc2 is colourful, not a black frame.
    assert!(img.pixels().any(|p| p.0[0] > 100) && img.pixels().all(|p| p.0[3] == 255));
}

#[test]
fn frame_after_video_ended_but_audio_continues() {
    // A mic track a little longer than the picture makes the container (and Probe::duration)
    // outlast the last video frame; a scrubber anywhere past it must still get a picture.
    let Some(fx) = Fixture::custom("longaudio", "long audio.mp4", "320x180", 1, &["-pix_fmt", "yuv420p", "-g", "10"]) else {
        return;
    };
    let p = probe(&fx.clip).unwrap();
    assert!(p.duration > 1.9, "{p:?}");
    assert_eq!(frame(&fx.clip, 1.5, 0).unwrap().dimensions(), (320, 180));
    assert_eq!(frame(&fx.clip, p.duration, 160).unwrap().dimensions(), (160, 90));
}

#[test]
fn compress_h264_and_hevc() {
    let Some(fx) = Fixture::new("compress") else { return };
    let input = probe(&fx.clip).unwrap();
    for (codec, tag) in [(Codec::H264, "avc1"), (Codec::Hevc, "hvc1")] {
        let out = fx.out(&format!("{codec:?}.mp4"));
        let opts = CompressOptions { codec, max_width: Some(321), ..Default::default() };
        let seen = recording(|p, c| compress(&fx.clip, &out, &opts, p, c));
        assert_progress_sane(&seen);
        let p = probe(&out).unwrap();
        // 321 → 320 wide (even), height follows the aspect and is even too.
        assert_eq!((p.width, p.height), (320, 180), "{codec:?}");
        assert!((p.duration - 2.).abs() < 0.1 && (p.fps - 30.).abs() < 0.01 && p.has_audio, "{codec:?} {p:?}");
        assert!(p.size_bytes > 1000 && p.size_bytes < input.size_bytes, "{codec:?} {} vs {}", p.size_bytes, input.size_bytes);
        assert_eq!(stream_field(&out, "codec_tag_string"), tag);
        assert_eq!(stream_field(&out, "pix_fmt"), "yuv420p");
    }
    assert_eq!(fx.others(), ["H264.mp4", "Hevc.mp4"]);
}

#[test]
fn compress_odd_444_input_to_even_420() {
    // Odd window sizes and 4:4:4 are legal for H.264 High 4:4:4 but would break most players.
    let Some(fx) = Fixture::custom("odd", "odd.mp4", "641x361", 2, &["-pix_fmt", "yuv444p"]) else { return };
    let out = fx.out("even.mp4");
    let opts = CompressOptions { fps: Some(10), ..Default::default() };
    compress(&fx.clip, &out, &opts, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    let p = probe(&out).unwrap();
    assert_eq!((p.width, p.height), (640, 360));
    assert!((p.fps - 10.).abs() < 0.01, "{p:?}");
    assert_eq!(stream_field(&out, "pix_fmt"), "yuv420p");
}

#[test]
fn compress_drops_audio() {
    let Some(fx) = Fixture::new("noaudio") else { return };
    let out = fx.out("silent.mp4");
    let opts = CompressOptions { keep_audio: false, ..Default::default() };
    compress(&fx.clip, &out, &opts, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    assert!(!probe(&out).unwrap().has_audio);
}

#[test]
fn compress_trims() {
    let Some(fx) = Fixture::new("trim") else { return };
    let out = fx.out("trimmed.mp4");
    let opts = CompressOptions { trim: Some(Trim { start: 0.5, end: 1.5 }), ..Default::default() };
    let seen = recording(|p, c| compress(&fx.clip, &out, &opts, p, c));
    assert_progress_sane(&seen);
    let p = probe(&out).unwrap();
    assert!((p.duration - 1.).abs() < 0.1, "{p:?}");
    assert!(p.has_audio);

    // An empty or inverted trim is refused before ffmpeg runs.
    let bad = CompressOptions { trim: Some(Trim { start: 1.5, end: 0.5 }), ..Default::default() };
    assert!(compress(&fx.clip, &fx.out("bad.mp4"), &bad, &mut |_| {}, &AtomicBool::new(false)).is_err());
    assert_eq!(fx.others(), ["trimmed.mp4"]);
}

#[test]
fn gif_is_valid_and_sized() {
    let Some(fx) = Fixture::new("gif") else { return };
    let out = fx.out("anim.gif");
    let opts = GifOptions { width: Some(320), ..Default::default() };
    let seen = recording(|p, c| gif(&fx.clip, &out, &opts, p, c));
    assert_progress_sane(&seen);
    let bytes = fs::read(&out).unwrap();
    assert_eq!(&bytes[..6], b"GIF89a");
    let p = probe(&out).unwrap();
    assert_eq!((p.width, p.height), (320, 180));
    assert!((p.duration - 2.).abs() < 0.15, "{p:?}");
    // The NETSCAPE2.0 application block is what makes a GIF loop.
    let loops = |b: &[u8]| b.windows(11).any(|w| w == b"NETSCAPE2.0");
    assert!(loops(&bytes));

    let once = fx.out("once.gif");
    let opts = GifOptions { width: None, loop_forever: false, trim: Some(Trim { start: 1., end: 2. }), ..Default::default() };
    gif(&fx.clip, &once, &opts, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    let bytes = fs::read(&once).unwrap();
    assert!(!loops(&bytes));
    let p = probe(&once).unwrap();
    assert_eq!(p.width, 640);
    assert!((p.duration - 1.).abs() < 0.15, "{p:?}");
}

#[test]
fn cancel_leaves_nothing_behind() {
    let Some(fx) = Fixture::new("cancel") else { return };
    let run = |out: &Path, which: &str| {
        let cancel = AtomicBool::new(false);
        let mut calls = 0;
        let mut progress = |_| {
            calls += 1;
            cancel.store(true, Ordering::Relaxed);
        };
        let err = match which {
            "gif" => gif(&fx.clip, out, &GifOptions::default(), &mut progress, &cancel),
            _ => compress(&fx.clip, out, &CompressOptions::default(), &mut progress, &cancel),
        }
        .unwrap_err();
        assert!(is_cancelled(&err) && err.is::<Cancelled>(), "{err:#}");
        assert_eq!(calls, 1);
    };

    let fresh = fx.out("fresh.mp4");
    run(&fresh, "mp4");
    run(&fx.out("fresh.gif"), "gif");
    assert_eq!(fx.others(), Vec::<String>::new());

    // A previous result at the target survives a cancelled re-run untouched.
    let existing = fx.out("existing.mp4");
    fs::write(&existing, "previous result").unwrap();
    run(&existing, "mp4");
    assert_eq!(fs::read_to_string(&existing).unwrap(), "previous result");
    assert_eq!(fx.others(), ["existing.mp4"]);
}

#[test]
fn failure_reports_ffmpeg_error_and_keeps_target() {
    let Some(fx) = Fixture::new("fail") else { return };
    // Output in a directory that doesn't exist: ffmpeg itself fails, after probing went fine.
    let err = compress(&fx.clip, &fx.out("missing/out.mp4"), &CompressOptions::default(), &mut |_| {}, &AtomicBool::new(false))
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("ffmpeg failed") && msg.contains("No such file or directory"), "{msg}");
    assert!(!is_cancelled(&err));
}

#[test]
fn names_with_spaces_and_umlauts() {
    let Some(fx) = Fixture::custom("names", "Shotty 2026-10-08 at 13.10.00 ü.mp4", "640x360", 2, &["-pix_fmt", "yuv420p"])
    else {
        return;
    };
    assert_eq!(probe(&fx.clip).unwrap().width, 640);
    assert_eq!(frame(&fx.clip, 1., 64).unwrap().width(), 64);
    let mp4 = fx.out("Shotty 2026-10-08 at 13.10.00 ü (klein).mp4");
    let opts = CompressOptions { max_width: Some(320), ..Default::default() };
    compress(&fx.clip, &mp4, &opts, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    assert_eq!(probe(&mp4).unwrap().width, 320);
    let gif_out = fx.out("Grüße & 'quotes' $HOME.gif");
    gif(&fx.clip, &gif_out, &GifOptions { width: Some(200), ..Default::default() }, &mut |_| {}, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(probe(&gif_out).unwrap().width, 200);
}

#[test]
fn in_place_compress_replaces_input() {
    let Some(fx) = Fixture::new("inplace") else { return };
    let before = probe(&fx.clip).unwrap();
    let opts = CompressOptions { max_width: Some(320), ..Default::default() };
    compress(&fx.clip, &fx.clip, &opts, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    let after = probe(&fx.clip).unwrap();
    assert_eq!(after.width, 320);
    assert!(after.size_bytes < before.size_bytes);
    assert_eq!(fx.others(), Vec::<String>::new());
}

#[test]
fn parse_probe_handles_cover_art_and_unknowns() {
    let text = "[STREAM]\ncodec_type=video\nwidth=300\nheight=300\navg_frame_rate=0/0\nr_frame_rate=90000/1\nDISPOSITION:attached_pic=1\n[/STREAM]\n\
                [STREAM]\ncodec_type=video\nwidth=2400\nheight=1500\navg_frame_rate=30000/1001\nr_frame_rate=60/1\nduration=N/A\nDISPOSITION:attached_pic=0\n[/STREAM]\n\
                [STREAM]\ncodec_type=audio\nchannels=1\n[/STREAM]\n[FORMAT]\nduration=N/A\n[/FORMAT]\n";
    let info = parse_probe(text, 42).unwrap();
    assert_eq!(
        info.probe,
        Probe { duration: 0., width: 2400, height: 1500, fps: 30000. / 1001., has_audio: true, size_bytes: 42 }
    );
    assert_eq!((info.max_fps, info.audio_channels), (60., 1));
    assert!(parse_probe("[STREAM]\ncodec_type=audio\n[/STREAM]\n", 1).is_err());
}

#[test]
fn quality_maps_to_crf() {
    assert_eq!(Codec::H264.crf(0), 38.);
    assert!((Codec::H264.crf(60) - 23.6).abs() < 1e-4);
    assert_eq!(Codec::H264.crf(100), 14.);
    assert_eq!(Codec::H264.crf(255), 14.);
    assert!((Codec::Hevc.crf(60) - 28.6).abs() < 1e-4);
}

#[test]
fn filters_keep_dimensions_even() {
    let info = |w, h, fps| Info {
        probe: Probe { duration: 1., width: w, height: h, fps, has_audio: false, size_bytes: 0 },
        max_fps: fps,
        audio_channels: 2,
    };
    let f = |i: &Info, o: CompressOptions| video_filter(i, &o);
    let d = CompressOptions::default;
    assert_eq!(f(&info(2400, 1500, 30.), d()), "format=yuv420p");
    assert_eq!(
        f(&info(2400, 1500, 30.), CompressOptions { max_width: Some(1200), ..d() }),
        "scale=1200:750:flags=lanczos,format=yuv420p"
    );
    // 1001×667 → 1000 wide; 666.33 rounds to 666.
    assert_eq!(f(&info(1001, 667, 30.), CompressOptions { max_width: Some(1001), ..d() }), "crop=1000:666:0:0,format=yuv420p");
    assert_eq!(
        f(&info(1000, 667, 30.), CompressOptions { max_width: Some(999), ..d() }),
        "scale=998:666:flags=lanczos,format=yuv420p"
    );
    // A cap only applies below the source rate; it must never duplicate frames.
    assert_eq!(f(&info(640, 360, 30.), CompressOptions { fps: Some(60), ..d() }), "format=yuv420p");
    assert_eq!(f(&info(640, 360, 60.), CompressOptions { fps: Some(30), ..d() }), "fps=30,format=yuv420p");
}
