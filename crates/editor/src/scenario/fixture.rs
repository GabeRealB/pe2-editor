//! Projects for scenarios, written where each scenario has its own: nothing here comes from a disc.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// The layer fixtures write to.
const LAYER: &str = "base";

/// A project in a folder of its own, gone with it.
pub struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        super::isolate();
        let fixture = Fixture { root: super::folder().join(format!("project-{}", NEXT.fetch_add(1, Ordering::Relaxed))) };
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

    /// A node as a template makes one, with these settings: a fixture has the stacks the editor would give it.
    #[must_use]
    pub fn stamped(self, path: &str, uid: &str, (template, file): (&str, &str), settings: &str) -> Fixture {
        let extensions = extensions::shared();
        let found = extensions.template(template).unwrap_or_else(|| panic!("no template {template}"));
        let stack = extensions.stamp(found, file).expect("a stack");
        let text = project::graph::layers::settings_on_layers(&format!("{settings}\n\n{stack}")).expect("a declaration");
        self.node(path, &format!("uid = \"uid://{uid}\"\n\n{text}"))
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
"#,
        )
    }

    /// A doll: a texture from a file, a skeleton, a mesh on it, a material, a model and a prefab holding it.
    #[must_use]
    pub fn doll() -> Fixture {
        const JOINTS: &str = r#"joints = [
    { name = "root", translation = [0, 0, 0], rotation = [0, 0, 0] },
    { name = "hip", parent = "root", translation = [0, -100, 0], rotation = [0, 0, 0] },
]"#;
        const PREFAB: &str = r#"skeleton = { slot = "uid://bones" }
parts.body = {}
groups.outfit.plain.body = { model = { slot = "uid://body" } }"#;
        Fixture::new()
            .stamped("doll/skin", "skin", ("texture", "skin.ora"), "")
            .bytes("doll/skin", "skin.ora", &painting())
            .stamped("doll/skeleton", "bones", ("skeleton", ""), JOINTS)
            .stamped("doll/shape", "shape", ("mesh", ""), "skeleton = { slot = \"uid://bones\" }")
            .bytes("doll/shape", "mesh.usda", MESH.as_bytes())
            .stamped("doll/paint", "paint", ("material", ""), "shader = \"textured\"\nsurface = \"opaque\"\nblend = \"add\"\nmain = \"uid://skin#cells/page_0@default\"")
            .stamped("doll/body", "body", ("model", ""), "mesh = { slot = \"uid://shape\" }\nmaterials.page_0 = { slot = \"uid://paint\" }")
            .stamped("doll/doll", "doll", ("prefab", ""), PREFAB)
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
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A picture as a tool would save one: an `.ora` of 16 colors, a texel layer and a cell.
#[must_use]
pub fn painting() -> Vec<u8> {
    use codecs::ora::{Document, Group, Layer, Node, write};
    use codecs::texture::rgba_png;
    let layer = |name: &str, png: &[u8]| Node::Layer(Layer { name: name.to_string(), x: 0, y: 0, visible: true, png: png.to_vec() });
    let group = |name: &str, children| Node::Group(Group { name: name.to_string(), visible: true, children });
    // Each channel full or empty, as those are exact in the 5 bits a color has.
    let full = |on: bool| if on { 255u8 } else { 0 };
    let colors: Vec<u8> = (0u8..16).map(|at| at % 7 + 1).flat_map(|c| [full(c & 1 != 0), full(c & 2 != 0), full(c & 4 != 0), 255]).collect();
    let strip = rgba_png(16, 1, &colors).expect("a strip");
    let texels: Vec<u8> = (0..64).flat_map(|at: usize| colors[at % 16 * 4..at % 16 * 4 + 4].to_vec()).collect();
    let picture = rgba_png(8, 8, &texels).expect("a picture");
    let stack = vec![group("palettes", vec![layer("default", &strip)]), group("cells", vec![layer("page_0", &picture)]), layer("texels", &picture)];
    write(&Document { width: 8, height: 8, stack }, &picture, &picture)
}

/// A square and a triangle, one on each joint, with one slot.
const MESH: &str = r#"#usda 1.0
(
    defaultPrim = "Model"
    metersPerUnit = 0.001
    upAxis = "Y"
)

def Xform "Model"
{
    custom token[] slots = ["page_0"]
    float xformOp:rotateX = 180
    uniform token[] xformOpOrder = ["xformOp:rotateX"]

    def SkelRoot "Rig" (
        prepend apiSchemas = ["SkelBindingAPI"]
    )
    {
        rel skel:skeleton = </Model/Rig/Skeleton>

        def Skeleton "Skeleton"
        {
            uniform token[] joints = ["part0", "part1"]
            uniform matrix4d[] bindTransforms = [((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1)), ((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1))]
            uniform matrix4d[] restTransforms = [((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1)), ((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1))]
        }

        def Mesh "Mesh" (
            prepend apiSchemas = ["SkelBindingAPI", "MaterialBindingAPI"]
        )
        {
            uniform bool doubleSided = 1
            uniform token orientation = "leftHanded"
            point3f[] points = [(0, 0, 0), (10, 0, 0), (10, 10, 0), (0, 10, 0), (20, 0, 0)]
            int[] faceVertexCounts = [4, 3]
            int[] faceVertexIndices = [0, 1, 2, 3, 1, 4, 2]
            custom float3[] sourceNormals = [(0, 0, 1)]
            normal3f[] primvars:normals = [(0, 0, 1)] (
                interpolation = "faceVarying"
            )
            int[] primvars:normalIndex = [0, 0, 0, 0, 0, 0, 0] (
                interpolation = "faceVarying"
            )
            color3f[] primvars:displayColor = [(1, 1, 1), (1, 1, 1), (1, 1, 1), (1, 1, 1), (1, 1, 1), (1, 1, 1), (1, 1, 1)] (
                interpolation = "faceVarying"
            )
            int2[] primvars:texel = [(0, 0), (7, 0), (7, 7), (0, 7), (0, 0), (7, 0), (7, 7)] (
                interpolation = "faceVarying"
            )
            texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1), (0, 0), (1, 0), (1, 1)] (
                interpolation = "faceVarying"
            )
            token[] primvars:shading = ["smooth", "smooth"] (
                interpolation = "uniform"
            )
            int[] primvars:part = [0, 1] (
                interpolation = "uniform"
            )
            token[] primvars:colorSource = ["none", "none"] (
                interpolation = "uniform"
            )
            int[] primvars:skel:jointIndices = [0, 0, 0, 0, 1] (
                elementSize = 1
                interpolation = "vertex"
            )
            float[] primvars:skel:jointWeights = [1, 1, 1, 1, 1] (
                elementSize = 1
                interpolation = "vertex"
            )
            matrix4d primvars:skel:geomBindTransform = ((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1))
            rel skel:skeleton = </Model/Rig/Skeleton>

            def GeomSubset "material0" (
                prepend apiSchemas = ["MaterialBindingAPI"]
            )
            {
                uniform token elementType = "face"
                uniform token familyName = "materialBind"
                int[] indices = [0, 1]
                rel material:binding = </Model/Materials/material0>
            }
        }
    }

    def "Materials"
    {
        def Material "material0"
        {
            token inputs:slot = "page_0"
            token outputs:surface.connect = </Model/Materials/material0/Surface.outputs:surface>

            def Shader "Surface"
            {
                uniform token info:id = "UsdPreviewSurface"
                float inputs:roughness = 1
                token outputs:surface
            }
        }
    }
}
"#;
