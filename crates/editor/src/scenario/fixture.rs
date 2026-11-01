//! Projects for scenarios, written where each scenario has its own: nothing here comes from a disc.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// The layer fixtures write to.
const LAYER: &str = "base";

/// A project in a folder of its own, gone with it.
pub struct Fixture {
    root: PathBuf,
    /// Someone else's, and left as it is.
    given: bool,
}

impl Fixture {
    /// A project that is there already.
    #[must_use]
    pub fn given(root: &Path) -> Fixture {
        Fixture { root: root.to_path_buf(), given: true }
    }

    fn new() -> Fixture {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        super::isolate();
        let fixture = Fixture { root: super::folder().join(format!("project-{}", NEXT.fetch_add(1, Ordering::Relaxed))), given: false };
        fixture.write("project.toml", &format!("[[layer]]\nname = \"{LAYER}\"\n"));
        fixture
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.root().join(path);
        std::fs::create_dir_all(path.parent().expect("under the root")).expect("a folder");
        std::fs::write(path, text).expect("a file");
    }

    /// A node, by its path, of this declaration.
    #[must_use]
    pub fn node(self, path: &str, declaration: &str) -> Fixture {
        self.write(&format!(".layers/{LAYER}/{path}.node/node.toml"), declaration);
        self
    }

    /// A file of a node, whole.
    #[must_use]
    pub fn bytes(self, path: &str, file: &str, bytes: &[u8]) -> Fixture {
        let at = self.root.join(format!(".layers/{LAYER}/{path}.node/{file}"));
        std::fs::create_dir_all(at.parent().expect("a bundle")).expect("a folder");
        std::fs::write(at, bytes).expect("a file");
        self
    }

    /// A plain file, in a node of it.
    #[must_use]
    pub fn stored(self, path: &str, file: &str) -> Fixture {
        self.write(&format!(".layers/{LAYER}/{path}.node/{file}"), "held\n");
        self
    }

    /// A node with no field of its own: a stored file, with what is taken from it.
    #[must_use]
    pub fn notes() -> Fixture {
        let stack = "[[stack]]\nop = \"file\"\nfile = \"notes.txt\"\n\n\
                     [[stack]]\nop = \"output\"\nname = \"native\"\n\n\
                     [[stack]]\nop = \"output\"\nname = \"kept\"\n";
        Fixture::new().node("notes", stack).stored("notes", "notes.txt")
    }

    /// What runs outside the sandbox, in a layer of its own beside the first: the project has to be asked about it.
    #[must_use]
    pub fn untrusted(required: bool) -> Fixture {
        let bytes = extensions::built_apart("pe2_psx.wasm").expect("built with the others");
        let stack = "[[stack]]\nop = \"file\"\nfile = \"extension.wasm\"\n";
        let fixture = Fixture::new();
        let mut file = format!("[[layer]]\nname = \"{LAYER}\"\n\n[[layer]]\nname = \"mods\"\n");
        if required {
            file.push_str("\n[[extension]]\nid = \"pe2.psx\"\nrequired = true\n");
        }
        fixture.write("project.toml", &file);
        fixture.write(".layers/mods/tools/psx.node/node.toml", stack);
        std::fs::write(fixture.root.join(".layers/mods/tools/psx.node/extension.wasm"), bytes).expect("a component");
        fixture
    }

    /// The rig, with a test extension taken in for testing.
    #[must_use]
    pub fn tested() -> Fixture {
        let rig = Fixture::rig();
        let mut manifest = format!("[[layer]]\nname = \"{LAYER}\"\n");
        let mut tested = rig;
        for (id, name) in [("pe2.render.tests", "render"), ("pe2.images.tests", "images")] {
            let _ = writeln!(manifest, "\n[[extension]]\nid = \"{id}\"\ndev = true");
            let bytes = extensions::built_apart(&format!("pe2_{name}_tests.wasm")).expect("built with the others");
            let at = format!("tests/{name}");
            tested = tested.node(&at, "[[stack]]\nop = \"file\"\nfile = \"extension.wasm\"\n").bytes(&at, "extension.wasm", &bytes);
        }
        tested.write("project.toml", &manifest);
        tested
    }

    /// A project of two configs.
    #[must_use]
    pub fn two_configs() -> Fixture {
        let fixture = Fixture::new();
        fixture.write("project.toml", &format!("[[layer]]\nname = \"{LAYER}\"\n\n[config.usa]\n\n[config.japan]\n"));
        fixture
    }

    /// A skeleton of three joints.
    #[must_use]
    pub fn rig() -> Fixture {
        Fixture::new().node(
            "rig/skeleton",
            r#"[[stack]]
id = "skeleton"
op = "pe2.render@skeleton"
joints = [
    { name = "root", translation = [0, 0, 0], rotation = [0, 0, 0] },
    { name = "pelvis", parent = "root", translation = [0, -927, 0], rotation = [0, 0, 0] },
    { name = "waist", parent = "pelvis", translation = [0, -79, 3], rotation = [0, 0, 0] },
]

[[stack]]
op = "output"
name = "native"

[[stack]]
op = "pe2.render@assemble"
node = { from = "skeleton" }

[[stack]]
op = "preview"
name = "scene"
retain = "cache"
"#,
        )
    }

    /// The skeleton, shown twice: the second a tab of its own.
    #[must_use]
    pub fn tapped() -> Fixture {
        let rig = Fixture::rig();
        let file = rig.node_file("rig/skeleton");
        let text = std::fs::read_to_string(&file).expect("its declaration");
        std::fs::write(file, format!("{text}\n[[stack]]\nop = \"preview\"\nname = \"again\"\n")).expect("its declaration");
        rig
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where a node's declaration is.
    #[must_use]
    pub fn node_file(&self, path: &str) -> PathBuf {
        self.root().join(format!(".layers/{LAYER}/{path}.node/node.toml"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.given {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}
