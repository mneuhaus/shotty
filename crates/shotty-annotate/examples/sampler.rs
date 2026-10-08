//! Renders every shape onto a picture: `cargo run -p shotty-annotate --example sampler in.png out.png`
use shotty_annotate::*;
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let base = image::open(&args[1]).expect("input").to_rgba8();
    let c = PALETTE;
    let doc = Document {
        shapes: vec![
            Shape::Pixelate { area: Area { x: 60., y: 300., w: 500., h: 120. } },
            Shape::Arrow { from: pt(900., 700.), to: pt(1300., 420.), color: c[0], width: 10. },
            Shape::Rect { area: Area { x: 1320., y: 300., w: 600., h: 220. }, color: c[0], width: 8. },
            Shape::Ellipse { area: Area { x: 200., y: 800., w: 500., h: 260. }, color: c[4], width: 8. },
            Shape::Highlight { area: Area { x: 900., y: 900., w: 700., h: 60. }, color: c[2] },
            Shape::Line { from: pt(900., 1100.), to: pt(1600., 1100.), color: c[3], width: 8. },
            Shape::Text { at: pt(1000., 560.), text: "Hier klicken".into(), color: c[0], size: 44. },
            Shape::Step { center: pt(820., 720.), number: 1, color: c[4], radius: 30. },
            Shape::Step { center: pt(1700., 1000.), number: 12, color: c[2], radius: 30. },
        ],
        crop: Some(Area { x: 0., y: 200., w: 2100., h: 1000. }),
    };
    render(&base, &doc).save(&args[2]).expect("save");
}
