//! The annotated example in the README and on the website:
//! `cargo run --release -p shotty-annotate --example docs_sample <mock.png> <out.png>`
use shotty_annotate::*;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let base = image::open(&args[1]).expect("input image").to_rgba8();
    let [red, _, yellow, _, blue, ..] = PALETTE;
    let doc = Document {
        shapes: vec![
            Shape::Pixelate { area: Area { x: 838., y: 263., w: 292., h: 43. } },
            Shape::Highlight { area: Area { x: 378., y: 443., w: 372., h: 24. }, color: yellow },
            Shape::Text { at: pt(772., 421.), text: "Turn this on".into(), color: red, size: 26. },
            Shape::Arrow { from: pt(924., 442.), to: pt(1072., 442.), color: red, width: 6. },
            Shape::Rect { area: Area { x: 374., y: 577., w: 165., h: 63. }, color: blue, width: 5. },
            Shape::Step { center: pt(1138., 420.), number: 1, color: red, radius: 17. },
            Shape::Step { center: pt(548., 579.), number: 2, color: blue, radius: 17. },
        ],
        crop: Some(Area { x: 96., y: 60., w: 1088., h: 692. }),
    };
    render(&base, &doc).save(&args[2]).expect("write output");
}
