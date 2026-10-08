//! Records a 400×300 pt area at the top-left of the main display for 3 seconds.
fn main() -> anyhow::Result<()> {
    let display = shotty_capture::displays()?.into_iter().find(|d| d.frame.x == 0.0 && d.frame.y == 0.0).expect("main display");
    let opts = shotty_capture::RecordOptions {
        display_id: display.id,
        area: shotty_capture::Rect { x: 100.0, y: 100.0, width: 400.0, height: 300.0 },
        fps: 30,
        cursor: true,
        clicks: true,
        microphone: false,
        system_audio: false,
        path: "/tmp/shotty-record-test.mp4".into(),
    };
    let _ = std::fs::remove_file(&opts.path);
    let rec = shotty_capture::Recording::start(&opts)?;
    std::thread::sleep(std::time::Duration::from_secs(3));
    let path = rec.stop()?;
    println!("wrote {} ({} bytes)", path.display(), std::fs::metadata(&path)?.len());
    Ok(())
}
