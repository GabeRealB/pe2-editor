//! `pe2.psx`: the `PlayStation`, as what a package's build ends with - what is the platform's and no toolchain's. A build
//! (a graph the project defines: its programs `run` boxes, the tools' extensions offering them) links a package and cuts
//! its image; `finish` makes that the image the game loads - its `.pe2pkg`, or the executable with its PS-X EXE header,
//! of the release built. It runs no program.

use pe2_ext::ty::{self, Files, Registered, SourceTree};
use pe2_ext::{Call, File, Registry, Result, Table, Typed, Value, anyhow, bail};

/// The target this is, as a config names it.
const TARGET: &str = "psx";

pe2_ext::extension!(register);

/// A sector of the disc, which the executable's header and image fill whole.
const SECTOR: usize = 0x800;

/// Where the BIOS puts the stack: the top of the 2 MB of RAM, less a few words.
const STACK: u32 = 0x801F_FFF0;

/// The license string a release's region carries in its executable's header, at 0x4C.
const REGIONS: [(&str, &str); 3] = [
    ("USA", "Sony Computer Entertainment Inc. for North America area"),
    ("JAP", "Sony Computer Entertainment Inc. for Japan area"),
    ("PAL", "Sony Computer Entertainment Inc. for Europe area"),
];

/// What a language server is told a package's sources are read as: the macros its builds define.
const DEFINES: [&str; 3] = ["_LANGUAGE_C", "USE_INCLUDE_ASM", "VER_USA"];

/// Where an ELF's first loaded segment goes: the executable's load address, which its link script gives.
fn load_address(elf: &[u8]) -> Result<u32> {
    let word = |at: usize| -> Result<u32> {
        Ok(u32::from_le_bytes(elf.get(at..at + 4).ok_or_else(|| anyhow!("the ELF is cut short"))?.try_into()?))
    };
    let half = |at: usize| -> Result<usize> {
        Ok(usize::from(u16::from_le_bytes(
            elf.get(at..at + 2).ok_or_else(|| anyhow!("the ELF is cut short"))?.try_into()?,
        )))
    };
    let (table, size, count) = (usize::try_from(word(0x1C)?)?, half(0x2A)?, half(0x2C)?);
    for index in 0..count {
        let header = table + index * size;
        if word(header)? == 1 {
            return word(header + 8);
        }
    }
    bail!("the ELF loads nothing")
}

/// The executable: its header, every field of which follows from the link or this target - the entry point (the ELF's),
/// where the image loads and how much of it, padded to whole sectors, the stack, the region's license string - then the
/// image, padded.
fn exe_header(image: &[u8], elf: &[u8], address: u32, release: &str) -> Result<Vec<u8>> {
    if elf.get(..6) != Some(b"\x7fELF\x01\x01".as_slice()) {
        bail!("the linked image is no 32-bit little-endian ELF");
    }
    let entry = u32::from_le_bytes(elf[0x18..0x1C].try_into()?);
    let (_, region) = REGIONS
        .iter()
        .find(|(known, _)| *known == release)
        .ok_or_else(|| anyhow!("no region for the release {release}"))?;
    let size = image.len().div_ceil(SECTOR) * SECTOR;
    let mut out = b"PS-X EXE".to_vec();
    for field in [0, 0, entry, 0, address, u32::try_from(size)?, 0, 0, 0, 0, STACK, 0, 0, 0, 0, 0, 0] {
        out.extend(field.to_le_bytes());
    }
    out.extend(region.as_bytes());
    out.resize(SECTOR, 0);
    out.extend(image);
    out.resize(SECTOR + size, 0);
    Ok(out)
}

/// An image the game loads.
pub struct Package;

impl Registered for Package {
    const NAME: &str = "pe2.psx@image";
    const LABEL: &str = "Package image";
}

impl Files for Package {}

#[expect(clippy::unnecessary_wraps, reason = "an entry point may fail; this one does not")]
fn register(r: &mut Registry) -> Result<()> {
    let _ = r.value_type::<Package>();
    let _ = r
        .op("finish")
        .label("The image the game loads")
        .input::<ty::Any>("image")
        .input::<ty::Any>("elf")
        .input::<SourceTree>("sources")
        .output::<ty::File<Package>>()
        .run(finish);
    let _ = r
        .op("flags")
        .label("What a language server is told")
        .takes("package", &["pe2.packages@package"])
        .output::<ty::Table>()
        .provides("flags")
        .run(flags);
    let _ = r.extend("pe2.packages@package").of(Package::NAME).register();
    Ok(())
}

/// What a language server needs of this target to read a package's sources.
#[expect(clippy::unnecessary_wraps, reason = "an operation may fail; this one does not")]
fn flags(_: &mut Call) -> Result<Table> {
    let defines = DEFINES.iter().map(|define| Value::String((*define).to_string()));
    let mut table = Table::new();
    table.insert("triple".into(), "mips-linux-gnu".into());
    table.insert("defines".into(), Value::Array(defines.collect()));
    Ok(table)
}

/// `finish`: what a build linked and cut (`image`: the `.image` section's bytes; `elf`: the linked file), made the image
/// the game loads - `<package>.pe2pkg`, or for an executable (`target.psx.executable`) `<package>.exe`, the PS-X EXE
/// header before it, of the release the configuration builds.
fn finish(call: &mut Call) -> Result<Typed> {
    let fields = call.given();
    let image = call.take::<ty::Any>(0)?;
    let elf = call.take::<ty::Any>(1)?;
    let sources = call.take::<SourceTree>(2)?;
    let (pe2_ext::Data::Typed(image), pe2_ext::Data::Typed(elf)) = (image, elf) else {
        bail!("it is given no image and linked file")
    };
    let name = name_in(call, &sources)?;
    let bytes = call.read(image.file()?)?;
    if pe2_ext::targeted(&fields, Some(TARGET)).get("executable").and_then(Value::as_bool) != Some(true) {
        let made = File::new(pe2_ext::RelPath::new(&format!("{}.pe2pkg", file_name(&name)))?, bytes);
        return Ok(Typed { notes: sources.notes.clone(), ..Typed::new(Package::NAME, vec![made]) });
    }
    let release = call.config().and_then(|config| config.release).ok_or_else(|| anyhow!("no release is built"))?;
    let elf = call.read(elf.file()?)?;
    let header = exe_header(&bytes, &elf, load_address(&elf)?, &release)?;
    let exe = File::new(pe2_ext::RelPath::new(&format!("{}.exe", file_name(&name)))?, header);
    Ok(Typed { notes: sources.notes.clone(), ..Typed::new(Package::NAME, vec![exe]) })
}

/// A file of the gathered tree, as text.
fn text_in(call: &Call, sources: &Typed, name: &pe2_ext::RelPath) -> Result<String> {
    let file =
        sources.files.iter().find(|file| *file.name == *name).ok_or_else(|| anyhow!("the tree has no {name}"))?;
    Ok(String::from_utf8_lossy(&call.read(file)?).into_owned())
}

/// A package's name, in the gathered tree.
fn name_in(call: &Call, sources: &Typed) -> Result<String> {
    text_in(call, sources, pe2_ext::RelPath::constant("package")).map_err(|_| anyhow!("the tree names no package"))
}

/// The file an image is written to: its package's name as the release names its files, the path's steps joined by `_` (`aya/mist`:
/// `aya_mist`).
fn file_name(name: &str) -> String {
    name.replace('/', "_")
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_executable_is_headed_as_the_bios_reads_it() {
        let mut elf = b"\x7fELF\x01\x01".to_vec();
        elf.resize(0x34, 0);
        elf[0x18..0x1C].copy_from_slice(&0x8001_4358u32.to_le_bytes());
        let made = super::exe_header(&[1, 2, 3], &elf, 0x8001_0000, "USA").unwrap();
        let word = |at: usize| u32::from_le_bytes(made[at..at + 4].try_into().unwrap());
        assert_eq!(&made[..8], b"PS-X EXE");
        assert_eq!((word(0x10), word(0x18), word(0x1C), word(0x30)), (0x8001_4358, 0x8001_0000, 0x800, 0x801F_FFF0));
        assert!(made[0x4C..].starts_with(b"Sony Computer Entertainment Inc. for North America area\0"));
        assert_eq!((made.len(), &made[0x800..0x803]), (0x1000, &[1, 2, 3][..]));
    }
}
