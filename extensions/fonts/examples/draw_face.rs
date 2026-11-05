//! A UFO face drawn as the game draws it: its outlines rasterized at a size, in the UI faces' style, then a line of text
//! and its glyphs written as PNGs.
//!
//! `cargo run --release -p pe2-fonts --example draw_face -- <face.ufo> <out dir> [em texels] [outline texels]`

use std::collections::BTreeMap;
use std::path::Path;

use pe2_fonts::raster::Raster;

/// What the sample says.
const TEXT: &str = "The quick brown fox jumps over the lazy dog.\nPACK MY BOX WITH FIVE DOZEN LIQUOR JUGS! 0123456789";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (folder, out) = (Path::new(&args[0]), Path::new(&args[1]));
    let number = |i: usize| args.get(i).map(|n| n.parse::<f64>().expect("a number"));
    let raster = Raster { pixels_per_em: number(2), outline: number(3).unwrap_or(1.0), ..Raster::default() };
    let mut files = BTreeMap::new();
    let mut todo = vec![folder.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for entry in std::fs::read_dir(&dir).expect("the UFO's folder") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                todo.push(path);
            } else {
                let name = path.strip_prefix(folder).expect("within it").to_string_lossy().replace('\\', "/");
                files.insert(name, std::fs::read(&path).expect("a file"));
            }
        }
    }
    let font = codecs::ufo::read(&files).expect("a UFO");
    let glyphs = pe2_fonts::face::from_ufo(&font, &raster).expect("its glyphs");
    let drawn = glyphs.iter().filter(|g| g.art.is_some()).count();
    println!("{} glyphs, {drawn} drawn", glyphs.len());
    std::fs::create_dir_all(out).expect("the out folder");
    for (name, image) in
        [("sample.png", pe2_fonts::render::lines(&glyphs, TEXT, 0)), ("glyphs.png", pe2_fonts::render::sheet(&glyphs))]
    {
        let mut png = Vec::new();
        let to_u32 = |n: usize| u32::try_from(n).expect("a size");
        let mut encoder = png::Encoder::new(&mut png, to_u32(image.width), to_u32(image.height));
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().and_then(|mut w| w.write_image_data(&image.rgba)).expect("a PNG");
        std::fs::write(out.join(name), png).expect("written");
    }
}
