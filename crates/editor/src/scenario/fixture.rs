//! Projects for scenarios, written where each scenario has its own: nothing here comes from a disc.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use super::suite::Suite;

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
        let fixture = Fixture {
            root: super::folder().join(format!("project-{}", NEXT.fetch_add(1, Ordering::Relaxed))),
            given: false,
        };
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

    /// A project of what a suite declares, holding its tests.
    #[must_use]
    pub fn for_suite(suite: &Suite) -> Fixture {
        const COMPONENT: &str = "[[stack]]\nop = \"file\"\nfile = \"extension.wasm\"\n";
        let declared = &suite.declared;
        let fixture = Fixture::new();
        let layers: Vec<&str> =
            if declared.layers.is_empty() { vec![LAYER] } else { declared.layers.iter().map(String::as_str).collect() };
        let mut file = String::new();
        for layer in &layers {
            let _ = writeln!(file, "[[layer]]\nname = \"{layer}\"\n");
            std::fs::create_dir_all(fixture.root.join(".layers").join(layer)).expect("a layer");
        }
        for config in &declared.configs {
            let _ = writeln!(file, "[config.{config}]\n");
        }
        if !declared.disabled.is_empty() {
            let _ = writeln!(file, "[extensions]\ndisabled = {:?}\n", declared.disabled);
        }
        fixture.write("project.toml", &file);
        let bundle = |name: &str| fixture.root.join(format!(".layers/{}/{name}.node", layers[0]));
        // What it tests with, and the tests with their data.
        for id in &declared.take {
            let bytes = extensions::built_apart(&format!("{}.wasm", id.replace('.', "_")))
                .unwrap_or_else(|| panic!("{id} was not built"));
            let at = bundle(&format!("extensions/{id}"));
            std::fs::create_dir_all(&at).expect("a bundle");
            std::fs::write(at.join("extension.wasm"), bytes).expect("a component");
            std::fs::write(at.join("node.toml"), COMPONENT).expect("a declaration");
        }
        let at = bundle(&format!("tests/{}", suite.name));
        std::fs::create_dir_all(at.join("data")).expect("a bundle");
        std::fs::copy(suite.folder.join("tests.wasm"), at.join("extension.wasm")).expect("its tests");
        std::fs::write(at.join("node.toml"), COMPONENT).expect("a declaration");
        for entry in std::fs::read_dir(suite.folder.join("data")).into_iter().flatten().flatten() {
            std::fs::copy(entry.path(), at.join("data").join(entry.file_name())).expect("its data");
        }
        if declared.trust {
            extensions::trust::trust(&fixture.root, std::slice::from_ref(&fixture.root)).expect("what is trusted");
        }
        fixture
    }

    /// An extension of the project's own carrying tests.
    #[must_use]
    pub fn with_tests() -> Fixture {
        let component = extensions::built_apart("pe2_psx.wasm").expect("built with the others");
        let tests = extensions::built_apart("pe2_render_tests.wasm").expect("built with the others");
        let suite = extensions::built_suites()
            .into_iter()
            .find(|suite| suite.ends_with("render/tests"))
            .expect("built with the others");
        let at = "extensions/psx";
        let fixture = Fixture::new()
            .node(at, "[[stack]]\nop = \"file\"\nfile = \"extension.wasm\"\n")
            .bytes(at, "extension.wasm", &component)
            .bytes(at, "tests/tests.wasm", &tests);
        let data = std::fs::read(suite.join("data/doll.usda")).expect("its data");
        fixture.bytes(at, "tests/data/doll.usda", &data)
    }

    /// A project of two configs.
    #[must_use]
    pub fn two_configs() -> Fixture {
        let fixture = Fixture::new();
        fixture.write("project.toml", &format!("[[layer]]\nname = \"{LAYER}\"\n\n[config.usa]\n\n[config.japan]\n"));
        fixture
    }

    /// A project whose code sits in a files layer, under the asset layer it is edited from.
    #[must_use]
    pub fn code() -> Fixture {
        let fixture = Fixture::new();
        fixture.write(
            "project.toml",
            &format!("[[layer]]\nname = \"code\"\nkind = \"files\"\n\n[[layer]]\nname = \"{LAYER}\"\n"),
        );
        fixture.write(&format!(".layers/{LAYER}/.keep"), "");
        fixture.write(".layers/code/src/thing.c", "int thing;\n");
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
        std::fs::write(file, format!("{text}\n[[stack]]\nop = \"preview\"\nname = \"again\"\n"))
            .expect("its declaration");
        rig
    }

    #[must_use]
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
