fn main() -> anyhow::Result<()> {
    for d in shotty_capture::displays()? { println!("display {:?}", d); }
    let shots = shotty_capture::capture_all_displays()?;
    for (i, s) in shots.iter().enumerate() {
        println!("shot {i}: {}x{} scale {}", s.image.width(), s.image.height(), s.scale());
        s.image.save(format!("/tmp/shotty-probe-{i}.png"))?;
    }
    for w in shotty_capture::windows()?.iter().take(5) { println!("window {} {:?} {} / {}", w.id, w.frame, w.app, w.title); }
    Ok(())
}
