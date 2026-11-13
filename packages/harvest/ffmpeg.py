"""ffmpeg.py <work> <source> <port> <host>...: FFmpeg's port's host files, from each host's build (<work>/build/ffmpeg-<host>).

For each host, `hosts/<host>/config/` gets what configure wrote that the sources include - `config.h` (its
FFMPEG_CONFIGURATION the options given, without what names this machine or makes it a cross build),
`config_components.h`, `config.asm`, `config_components.asm`, `libavutil/avconfig.h` and `ffversion.h`, the lists of
components - and `hosts/<host>/sources.zon` what make compiled and linked: the flags every C file and every arm64
assembly file shares, the library's C sources, its nasm and `.S` files, its sources generated while building, each
program's own objects, the libraries linked.

Every object's command is read from make's log (made with -O, so that no job's output splits another's) and held to
the objects on disk: a command the log lacks fails the harvest.
"""
import collections, glob, os, re, shlex, shutil, sys

WORK, SOURCE, PORT = (os.path.realpath(p) for p in sys.argv[1:4])
HOSTS = sys.argv[4:]
TOOLS = f"{WORK}/tools/bin/"
GENERATED = ("fftools/resources/graph.css.c", "fftools/resources/graph.html.c",
             "libswscale/aarch64/ops_lookup.gen.c", "libswscale/aarch64/ops_neon.gen.S")
CONFIG_FILES = ["config.h", "config_components.h", "config.asm", "config_components.asm", "libavutil/avconfig.h",
                "libavutil/ffversion.h", "libavcodec/bsf_list.c", "libavcodec/codec_list.c", "libavcodec/parser_list.c",
                "libavdevice/indev_list.c", "libavdevice/outdev_list.c", "libavfilter/filter_list.c",
                "libavformat/demuxer_list.c", "libavformat/muxer_list.c", "libavformat/protocol_list.c"]
# Objects built for the machine building (FFmpeg's own tools), and what configure leaves of its tests.
NOT_THE_HOSTS = {"ffbuild/bin2c_host.o", "libswscale/aarch64/ops_asmgen.o", "a.o"}
# What a configure option starting so says of the machine or the build, not of FFmpeg.
NOT_CONFIGURATION = ("--enable-cross-compile", "--target-os", "--arch", "--pkg-config", "--disable-doc", "--disable-debug",
                     "--disable-stripping", "--enable-static", "--disable-shared")


def analyze(build, sys_dir):
    """Each object's source, kind (c, S, asm) and flags; the flags of a kind every object shares; the link of ffmpeg."""
    objects, links = {}, {}
    for line in open(f"{build}/make.log"):
        line = line.strip()
        if not line.startswith(TOOLS) or " -o " not in line:
            continue
        w = shlex.split(line)
        tool = w[0].rsplit("/", 1)[-1]
        out = w[w.index("-o") + 1]
        if out.removesuffix(".exe") in ("ffmpeg_g", "ffprobe_g"):
            links[out.removesuffix(".exe")] = w
            continue
        if not out.endswith(".o"):
            continue
        src = w[-1] if tool == "nasm" else [x for x in w if x.endswith((".c", ".S"))][-1]
        kind = "asm" if tool == "nasm" else ("S" if src.endswith(".S") else "c")
        flags, i = [], 1
        while i < len(w):
            x = w[i]
            if x in ("-MF", "-MT", "-o"):
                i += 2
                continue
            if x in ("-c", "-MMD", "-MD", src) or x.endswith(".d"):
                i += 1
                continue
            flags.append(x.replace(SOURCE, "$SRC").replace(sys_dir, "$SYS"))
            i += 1
        src = src.replace(SOURCE + "/", "src/")
        objects[out] = {"src": src[len("src/"):] if src.startswith("src/") else src, "kind": kind, "flags": flags}
    on_disk = {os.path.relpath(f, build) for f in glob.glob(f"{build}/**/*.o", recursive=True)} - NOT_THE_HOSTS
    missing = on_disk - set(objects)
    if missing:
        raise SystemExit(f"{build}: the log lacks the commands of {sorted(missing)[:10]}")
    kinds = collections.defaultdict(list)
    for v in objects.values():
        kinds[v["kind"]].append(v)
    common = {}
    for k, vs in kinds.items():
        common[k] = [x for x in vs[0]["flags"] if all(x in v["flags"] for v in vs)]
        for v in vs:
            v["extra"] = [x for x in v["flags"] if x not in common[k]]
    return objects, common, links


def zon_list(name, items):
    if not items:
        return f"    .{name} = .{{}},\n"
    return f"    .{name} = .{{\n" + "".join(f'        "{i}",\n' for i in items) + "    },\n"


for host in HOSTS:
    build = f"{WORK}/build/ffmpeg-{host}"
    sys_dir = os.path.realpath(f"{WORK}/sys-{host}")
    out = f"{PORT}/hosts/{host}"
    shutil.rmtree(out, ignore_errors=True)
    for f in CONFIG_FILES:
        if not os.path.exists(f"{build}/{f}"):
            continue
        os.makedirs(os.path.dirname(f"{out}/config/{f}"), exist_ok=True)
        text = open(f"{build}/{f}").read()
        if f == "config.h":
            given = re.search(r'#define FFMPEG_CONFIGURATION "(.*)"', text).group(1).split()
            kept = [o for o in given if "/" not in o and not o.startswith(NOT_CONFIGURATION)]
            text = re.sub(r'#define FFMPEG_CONFIGURATION ".*"', f'#define FFMPEG_CONFIGURATION "{" ".join(kept)}"', text)
        if WORK in text:
            raise SystemExit(f"{host}: {f} names the harvest's work folder")
        open(f"{out}/config/{f}", "w").write(text)

    objects, common_flags, links = analyze(build, sys_dir)
    programs = {p: [w for w in links[p] if w.startswith("fftools/") and w.endswith(".o")] for p in ("ffmpeg_g", "ffprobe_g")}
    tools = sorted(set(programs["ffmpeg_g"]) & set(programs["ffprobe_g"]))
    library, nasm, assembly, generated = [], [], [], []
    for o, v in sorted(objects.items()):
        if o.startswith("fftools/"):
            continue
        if v["src"] in GENERATED:
            generated.append(v["src"])
        elif v["kind"] == "asm":
            nasm.append(v["src"])
        elif v["kind"] == "S":
            assembly.append(v["src"])
        else:
            assert "-DHAVE_AV_CONFIG_H" in v["extra"], o
            library.append(v["src"])
    libraries = []
    for w in links["ffmpeg_g"]:
        if w.startswith("-l") and w[2:] not in libraries and w[2:] not in (
                "avdevice", "avfilter", "avformat", "avcodec", "swresample", "swscale", "avutil", "atomic"):
            libraries.append(w[2:])

    def common(kind):
        """A kind's common flags as the port gives them: include folders of the sources apart (`src:`); the build
        folder's, other libraries' and warnings left out."""
        flags, include = [], []
        for f in common_flags.get(kind, []):
            if f.startswith("-I"):
                if f.startswith("-I$SRC/"):
                    include.append("src:" + f[len("-I$SRC/"):].rstrip("/"))
                continue
            if f.startswith(("-W", "-Werror")) or f in ("-Qunused-arguments", "-mdynamic-no-pic"):
                continue
            if f not in flags:
                flags.append(f)
        return flags, include

    c_flags, c_include = common("c")
    assembly_flags, _ = common("S")
    with open(f"{out}/sources.zon", "w") as z:
        z.write(f"// What FFmpeg's own build compiled and linked for {host}, configured as config/config.h says.\n.{{\n")
        z.write(zon_list("flags", c_flags))
        z.write(zon_list("include", c_include))
        z.write(zon_list("assembly_flags", assembly_flags))
        z.write(zon_list("library", library))
        z.write(zon_list("nasm", nasm))
        z.write(zon_list("assembly", assembly))
        z.write(zon_list("generated", generated))
        z.write(zon_list("tools", [objects[o]["src"] for o in tools]))
        z.write(zon_list("ffmpeg", [objects[o]["src"] for o in programs["ffmpeg_g"] if o not in tools]))
        z.write(zon_list("ffprobe", [objects[o]["src"] for o in programs["ffprobe_g"] if o not in tools]))
        z.write(zon_list("system_libraries", libraries))
        z.write("}\n")
    print(f"  {host}: {len(library)} C, {len(nasm)} nasm, {len(assembly)} .S, {len(generated)} generated, {len(libraries)} libraries")
