//! Every dialogue read and made again: `cargo run -p pe2-dialogue --example dialogue_identity -- <files.txt> <args.json>
//! [<out folder>]`. The files are the chunks the arguments name by position - a dialogue's own file stands for a bundle
//! holding it as resource 0 - and the arguments are `extract_dialogues`'s, as JSON.

fn main() {
    let mut args = std::env::args().skip(1);
    let (list, json) = (args.next().expect("files.txt"), args.next().expect("args.json"));
    let out = args.next();
    let chunks: Vec<Vec<u8>> = std::fs::read_to_string(&list)
        .expect("a list")
        .lines()
        .map(|path| {
            let bytes = std::fs::read(path).expect("a chunk");
            if std::path::Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("cap")) {
                let mut bundle = formats::bundle::Bundle { base: 0x8010_0000, ..Default::default() };
                bundle.resources.insert(0, (3, bytes));
                formats::bundle::write(&bundle)
            } else {
                bytes
            }
        })
        .collect();
    let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&json).expect("json")).expect("json");
    let table: toml::Table = toml::Value::try_from(value).expect("toml").as_table().cloned().expect("a table");
    let files = pe2_dialogue::extract_dialogues(&chunks, &table).expect("extracted");
    let (mut failed, mut read, mut unsettled) = (0, 0, 0);
    for (name, bytes) in &files {
        if !name.ends_with("settings.toml") || name.starts_with("strings") {
            continue;
        }
        let text = String::from_utf8_lossy(bytes);
        if text.starts_with("failed") {
            failed += 1;
            println!("{name}: {}", text.trim());
        } else {
            read += 1;
            if text.contains("unsettled") {
                unsettled += 1;
            }
        }
    }
    println!("{read} read exactly ({unsettled} with something unsettled), {failed} failed");
    if let Some(out) = out {
        for (name, bytes) in &files {
            let path = std::path::Path::new(&out).join(name);
            std::fs::create_dir_all(path.parent().expect("a folder")).expect("a folder");
            std::fs::write(path, bytes).expect("written");
        }
    }
}
