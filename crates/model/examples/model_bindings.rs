//! What a stored model's materials are: for each model given as `file offset size address` (a package as inflated, where
//! in it the model is and where it loads), one line of JSON with its slots' placements, its materials, its effect and
//! whether it reflects. What declarations of a model's materials are made from.
//!
//! `cargo run -p pe2-model --example model_bindings -- <an inflated package> 760 1504 2148652216`

use pe2_model::model::decode;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    for chunk in args.chunks(4) {
        let [file, offset, size, address] = chunk else { continue };
        let number = |text: &str| text.parse::<usize>().unwrap_or_default();
        let bytes = std::fs::read(file).unwrap_or_default();
        let (offset, size) = (number(offset), number(size));
        let Some(blob) = bytes.get(offset..offset + size) else {
            println!("{{\"error\": \"past the end of {file}\"}}");
            continue;
        };
        match decode(blob, u32::try_from(number(address)).unwrap_or_default()) {
            Ok(model) => {
                let line = serde_json::json!({
                    "slots": model.slots,
                    "materials": model.materials,
                    "effect": model.effect,
                    "reflection": model.reflection,
                    "parts": model.parts.len(),
                });
                println!("{line}");
            }
            Err(why) => println!("{}", serde_json::json!({ "error": format!("{why:#}") })),
        }
    }
}
