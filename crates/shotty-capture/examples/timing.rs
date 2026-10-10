//! How long from the shortcut to a frozen frame? Prints a few rounds of the capture hot path.
use std::time::Instant;

fn ms(t: Instant) -> String {
    format!("{:>6.1} ms", t.elapsed().as_secs_f64() * 1000.)
}

fn main() -> anyhow::Result<()> {
    shotty_capture::warm_up();
    for round in 0..3 {
        println!("--- round {round}");
        let t = Instant::now();
        let displays = shotty_capture::displays()?;
        println!("displays() ×{}          {}", displays.len(), ms(t));
        let t = Instant::now();
        let windows = shotty_capture::windows()?;
        println!("windows() ×{}          {}", windows.len(), ms(t));
        let t = Instant::now();
        let frozen = shotty_capture::freeze_displays()?;
        println!("freeze_displays()      {}", ms(t));
        for f in frozen {
            f.pixels()?;
        }
        println!("  + pixels             {}", ms(t));
    }
    Ok(())
}
