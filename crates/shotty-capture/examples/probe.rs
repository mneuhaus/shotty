fn main() -> anyhow::Result<()> {
    for d in shotty_capture::displays()? {
        println!("display {:?} scale {}", d, shotty_capture::scale_of(d.id)?);
    }
    let shots = shotty_capture::capture_all_displays()?;
    for (i, s) in shots.iter().enumerate() {
        println!("shot {i}: {}x{} scale {}", s.bgra.width(), s.bgra.height(), s.scale());
        s.to_rgba().save(format!("/tmp/shotty-probe-{i}.png"))?;
    }
    for w in shotty_capture::windows()?.iter().take(5) { println!("window {} {:?} {} / {}", w.id, w.frame, w.app, w.title); }
    Ok(())
}
