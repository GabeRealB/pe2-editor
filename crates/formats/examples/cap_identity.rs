//! Every CAP dialogue in a folder read and written back: `cargo run -p formats --example cap_identity -- <folder>`.

fn main() {
    let folder = std::env::args().nth(1).expect("a folder of .cap files");
    let mut entries: Vec<_> = std::fs::read_dir(&folder).expect("a folder").flatten().map(|e| e.path()).collect();
    entries.sort();
    let (mut same, mut refused) = (0, 0);
    for path in entries.iter().filter(|p| p.extension().is_some_and(|e| e == "cap")) {
        let bytes = std::fs::read(path).expect("a file");
        match formats::cap::parse(&bytes) {
            Ok(cap) if formats::cap::write(&cap) == bytes => same += 1,
            Ok(_) => println!("{}: differs", path.display()),
            Err(why) => {
                refused += 1;
                println!("{}: {why}", path.display());
            }
        }
    }
    println!("{same} read back as written, {refused} refused");
}
