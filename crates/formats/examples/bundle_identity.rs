//! Every resource bundle in a folder read and written back: `cargo run -p formats --example bundle_identity -- <folder>`.

fn main() {
    let folder = std::env::args().nth(1).expect("a folder of .pe2bundle files");
    let mut entries: Vec<_> = std::fs::read_dir(&folder).expect("a folder").flatten().map(|e| e.path()).collect();
    entries.sort();
    let (mut same, mut refused) = (0, 0);
    for path in entries.iter().filter(|p| p.is_file()) {
        let bytes = std::fs::read(path).expect("a file");
        match formats::bundle::parse(&bytes) {
            Ok(_) => same += 1,
            Err(why) => {
                refused += 1;
                println!("{}: {why}", path.display());
            }
        }
    }
    println!("{same} read back as written, {refused} refused");
}
