//! Whether the UI fonts' page and glyph tables are made again, byte for byte, from what they are read into.
//!
//! `cargo run --release -p pe2-fonts --example ui_identity -- <assets/USA> <rom/USA/disk1/SLUS_010.42>`
//!
//! The page (stage 0 file 20162's font image) and Medium's, Large's and Small's tables (the executable's
//! `_gFontGlyphs*`) are read into the faces' glyphs and the atlas's layout, made again from them, and compared.

use pe2_fonts::atlas::{self, FaceSpec, PageSpec};
use pe2_fonts::encoding::Encoding;
use std::path::Path;

/// The executable's load address and where its text starts in the file.
const LOAD: usize = 0x8001_0000;
const HEADER: usize = 0x800;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (assets, exe) = (Path::new(&args[0]), std::fs::read(&args[1]).expect("the executable"));
    let payload = std::fs::read(assets.join("raw/pe2img/ui/font/image.pe2img")).expect("the font page");
    let pages = codecs::texture::pages(&payload).expect("its pages");
    let texels: Vec<u8> = codecs::texture::texels(&pages[0], 4, (0, 0, 256, 256))
        .into_iter()
        .map(|t| u8::try_from(t).expect("4 bits"))
        .collect();
    let ui = Encoding::of(&toml::from_str("base = \"windows-1252\"\nfirst = 0x20\n[characters]\n\"0x7B\" = \"○\"\n\"0x7C\" = \"△\"\n\"0x7D\" = \"✕\"\n\"0x7E\" = \"□\"\n").unwrap()).unwrap();
    let mut small = ui.clone();
    for (code, c) in [(0x24, '○'), (0x25, '□'), (0x26, '△'), (0x2A, '✕')] {
        small.chars.insert(code, c);
    }
    let spec = |name: &str,
                count: usize,
                bias: u8,
                band: (usize, usize),
                encoding: &Encoding,
                missing: Option<(char, u32)>,
                nothing: &[u32]| FaceSpec {
        name: name.into(),
        first: 0x20,
        count,
        bias,
        band,
        encoding: encoding.clone(),
        missing,
        nothing: nothing.to_vec(),
        only: None,
        format: atlas::Format::FontGlyph,
    };
    let specs = [
        spec("medium", 224, 38, (38, 125), &ui, Some((' ', 0x7F)), &[0x81]),
        spec("large", 224, 128, (128, 255), &ui, Some((' ', 0x7F)), &[0x81]),
        spec("small", 91, 0, (0, 32), &small, None, &[]),
    ];
    let tables: Vec<&[u8]> = [(0x8005_EFB0, 224), (0x8005_FA30, 224), (0x8006_04B0, 91)]
        .iter()
        .map(|&(va, n)| &exe[va - LOAD + HEADER..va - LOAD + HEADER + n * atlas::RECORD])
        .collect();
    let page = PageSpec { width: 256, height: 256, background: 5, gap: 0, blank: Some((0, 0)), under: None, pages: 1 };
    let (glyphs, layout, _) = atlas::decode(&page, &texels, &specs, &tables).expect("decoded");
    for (spec, face) in specs.iter().zip(&glyphs) {
        let kept: Vec<String> = layout.faces[&spec.name].kept.keys().map(|c| format!("{c:#X}")).collect();
        let shared: Vec<String> = face.iter().filter(|g| g.chars.len() > 1).map(|g| g.chars.iter().collect()).collect();
        let clipped: Vec<&str> = face
            .iter()
            .filter(|g| {
                g.art.as_ref().is_some_and(|a| usize::from(g.width) != a.width || usize::from(g.height) != a.height)
            })
            .map(|g| g.name.as_str())
            .collect();
        println!(
            "{}: {} glyphs in {} rows; kept {kept:?}; shared {shared:?}; clipped {clipped:?}",
            spec.name,
            face.len(),
            layout.faces[&spec.name].rows.len()
        );
    }
    let read_back = through_ufo(&specs, &glyphs, args.get(2).map(Path::new));
    let layout = pe2_fonts::layout_of(&pe2_fonts::layout_text(&layout)).expect("the layout reads back");
    let (made, made_tables) = atlas::generate(&page, &specs, &read_back, &layout).expect("made");
    let differ = made.iter().zip(&texels).filter(|(a, b)| a != b).count();
    println!("page: {differ} texels differ");
    let mut failed = differ != 0;
    for ((spec, made), stored) in specs.iter().zip(&made_tables).zip(&tables) {
        let bad: Vec<String> = made
            .chunks(12)
            .zip(stored.chunks(12))
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, _)| format!("{:#X}", spec.first as usize + i))
            .collect();
        println!("{}: {} records differ {:?}", spec.name, bad.len(), &bad[..bad.len().min(8)]);
        failed |= !bad.is_empty();
    }
    packing(&page, &specs, &read_back, &layout);
    if differ != 0 {
        let at: Vec<(usize, usize, u8, u8)> = made
            .iter()
            .zip(&texels)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .take(10)
            .map(|(i, (a, b))| (i % 256, i / 256, *a, *b))
            .collect();
        println!("first texels (x, y, made, stored): {at:?}");
    }
    std::process::exit(i32::from(failed));
}

/// Each face's pins cleared and packed afresh in its band, as a new face's would be.
fn packing(page: &PageSpec, specs: &[FaceSpec], fonts: &[Vec<atlas::Glyph>], layout: &atlas::Layout) {
    for spec in specs {
        let mut cleared = layout.clone();
        cleared.faces.get_mut(&spec.name).expect("its layout").rows.clear();
        match atlas::pack(page, specs, fonts, &cleared).and_then(|packed| atlas::generate(page, specs, fonts, &packed))
        {
            Ok(_) => println!("{} packed: fits its band", spec.name),
            Err(why) => println!("{} packed: {why}", spec.name),
        }
    }
}

/// Through UFO: each face written as a font editor opens it (into `out`, where given), and read back.
fn through_ufo(specs: &[FaceSpec], glyphs: &[Vec<atlas::Glyph>], out: Option<&Path>) -> Vec<Vec<atlas::Glyph>> {
    let mut read_back = Vec::new();
    for (spec, face) in specs.iter().zip(glyphs) {
        let files = pe2_fonts::face::ufo_files(
            face,
            "Parasite Eve II UI",
            &spec.name,
            if spec.name == "small" { 1 } else { 2 },
        );
        if let Some(out) = out {
            for (name, bytes) in &files {
                let path = out.join(format!("{}.ufo", spec.name)).join(name);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, bytes).unwrap();
            }
        }
        let font = codecs::ufo::read(&files).expect("the UFO reads back");
        let again =
            pe2_fonts::face::from_ufo(&font, &pe2_fonts::raster::Raster::default()).expect("its glyphs read back");
        if again != *face {
            let first = again.iter().zip(face).find(|(a, b)| a != b);
            println!("{}: the UFO reads back differently: {first:?}", spec.name);
        }
        read_back.push(again);
    }
    read_back
}
