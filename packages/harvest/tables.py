"""tables.py <log> <src dir> <build dir> <port host dir> <archive>[=<name>]...

What a library's own build compiled, from the harvest log the logging wrappers wrote while it was made, as the port's
files for one host: <port host dir>/sources.zon, the objects of each archive named (by the name given, else the
archive's) as groups of sources compiled alike, and <port host dir>/build/, the files its build generated that the
sources read (headers, generated sources), copied from the build folder.

Paths are written `src:<path>` (in the source tree) or `build:<path>` (generated); include folders outside both (other
libraries' installs) are left out, as the port links those libraries instead.
"""
import collections, os, shutil, subprocess, sys

LOG, SRC, BUILD, OUT = (os.path.realpath(p) for p in sys.argv[1:5])
ARCHIVES = sys.argv[5:]
# The harvest's work folder, which nothing written may name.
WORK = os.path.realpath(os.environ["WORK"])

records = []
for r in open(LOG, "rb").read().split(b"\x1e")[1:]:
    f = r.decode().split("\x1f")[:-1]
    records.append((f[0], f[1], f[2:]))


def where(path, cwd):
    """A path as the port names it: src:<rel>, build:<rel>, or None if it is neither."""
    p = os.path.realpath(os.path.join(cwd, path))
    for root, prefix in ((BUILD, "build:"), (SRC, "src:")):  # build first: it may sit inside the source tree
        if p == root:
            return prefix
        if p.startswith(root + os.sep):
            return prefix + os.path.relpath(p, root)
    return None


def definitions(path):
    """A header's definitions as -D flags, if it holds nothing else (comments, #pragma once, #define)."""
    import re
    text = re.sub(r"/\*.*?\*/", "", open(path).read(), flags=re.S)
    flags = []
    for line in text.splitlines():
        line = line.strip()
        if not line or line == "#pragma once":
            continue
        m = re.fullmatch(r"#define\s+([A-Za-z_][A-Za-z0-9_]*)(?:\s+(.*))?", line)
        if m is None:
            return None
        flags.append(f"-D{m.group(1)}" + (f"={m.group(2)}" if m.group(2) else ""))
    return flags


DROP_ALONE = {"-c", "-MD", "-MMD", "-MP", "-M", "-pipe", "-fdiagnostics-color", "-fcolor-diagnostics", "-Qunused-arguments"}
DROP_WITH_VALUE = {"-o", "-MF", "-MT", "-MQ", "-target", "-isysroot", "-arch", "-Xclang"}
PATH_FLAGS = {"-include", "-imacros", "-P"}

objects = {}  # object (absolute) -> (kind, flags, includes, path flags, source)
for tool, cwd, args in records:
    if tool not in ("cc", "c++", "nasm") or ("-o" not in args and "-c" not in args):
        continue
    if "-o" in args:
        out = os.path.realpath(os.path.join(cwd, args[args.index("-o") + 1]))
    else:  # compiled to <source's name>.o where it is run
        src_arg = next((a for a in args if not a.startswith("-") and a.endswith((".c", ".cc", ".cpp", ".cxx", ".S", ".s"))), None)
        if src_arg is None:
            continue
        out = os.path.realpath(os.path.join(cwd, os.path.splitext(os.path.basename(src_arg))[0] + ".o"))
    if not out.endswith((".o", ".obj", ".lo")) or ("-c" not in args and tool != "nasm"):
        continue
    flags, includes, path_flags, source, language = [], [], [], None, None
    i = 0
    while i < len(args):
        a = args[i]
        if a == "-x" and tool != "nasm":  # the language the sources are compiled as, whatever their names say
            language = args[i + 1]; i += 2; continue
        if a in DROP_WITH_VALUE or (tool == "nasm" and a in ("-MD", "-MF", "-MT", "-MQ")):
            i += 2; continue
        if a in DROP_ALONE or a.startswith(("-W", "-g", "-fdiagnostics", "-mmacosx-version-min", "-MF", "-MT", "-MQ")) and not a.startswith("-Wa,"):
            i += 1; continue
        if a.startswith(("-fmacro-prefix-map=", "-ffile-prefix-map=", "-fdebug-prefix-map=")):
            i += 1; continue  # paths of the machine the build ran on, as __FILE__ and debug information give them
        if tool == "nasm" and a == "-i":
            a = "-I"  # nasm's other spelling of an include folder (openh264's)
        for opt in ("-I", "-isystem", "-iquote"):
            if a == opt or (a.startswith(opt) and opt == "-I"):
                value = args[i + 1] if a == opt else a[len(opt):]
                place = where(value, cwd)
                if place is not None and place not in includes:
                    includes.append(place)
                i += 2 if a == opt else 1
                break
        else:
            if a in PATH_FLAGS or (tool == "nasm" and a.startswith("-P") and len(a) > 2):
                value = args[i + 1] if a in PATH_FLAGS else a[2:]
                place = where(value, cwd)
                if place is None:
                    raise SystemExit(f"{out}: {a} names {value}, outside the source and build folders")
                # A pre-included file of definitions alone (meson's config.h) is given as what it defines: zig's
                # cache reads a pre-included file's name as a path from where it runs.
                defines = definitions(os.path.join(cwd, value)) if a == "-include" else None
                if defines is not None:
                    flags += defines
                    i += 2
                    continue
                path_flags += ["-P" if a.startswith("-P") else a, place]
                i += 2 if a in PATH_FLAGS else 1
                continue
            if not a.startswith("-") and a.endswith((".c", ".cc", ".cpp", ".cxx", ".S", ".s", ".asm", ".m")):
                source = where(a, cwd)
                if source is None:
                    raise SystemExit(f"{out}: compiles {a}, outside the source and build folders")
                i += 1; continue
            flags.append(a); i += 1
    if source is None:
        raise SystemExit(f"{out}: no source in {args}")
    ext = source.rsplit(".", 1)[-1]
    kind = "nasm" if tool == "nasm" else "assembly" if ext in ("S", "s") else "cpp" if ext in ("cc", "cpp", "cxx") else "c"
    if language is not None:
        kind = {"c": "c", "c++": "cpp", "assembler-with-cpp": "assembly"}[language]
    for f in flags:
        if WORK in f or os.environ["WORK"] in f:
            raise SystemExit(f"{out}: its flag {f} names the harvest's work folder")
    objects[out] = (kind, tuple(flags), tuple(includes), tuple(path_flags), source)

members = collections.defaultdict(list)
for tool, cwd, args in records:
    if tool != "ar":
        continue
    archive = next((a for a in args[1:] if a.endswith((".a", ".lib"))), None)
    if archive is None:
        continue
    archive = os.path.realpath(os.path.join(cwd, archive))
    for a in args[args.index(next(x for x in args[1:] if x.endswith((".a", ".lib")))) + 1:]:
        if a.startswith("@"):
            for line in open(os.path.join(cwd, a[1:])).read().split():
                members[archive].append(os.path.realpath(os.path.join(cwd, line)))
        else:
            members[archive].append(os.path.realpath(os.path.join(cwd, a)))


def zon_strings(items, indent):
    if not items:
        return ".{}"
    pad = " " * indent
    quote = lambda x: '"' + x.replace("\\", "\\\\").replace('"', '\\"') + '"'
    return ".{\n" + "".join(f'{pad}    {quote(x)},\n' for x in items) + f"{pad}}}"


shutil.rmtree(OUT, ignore_errors=True)
os.makedirs(OUT)
lines = [f"// What the library's own build compiled for this host, harvested from it: each library's sources, in groups\n"
         f"// compiled alike. `src:` names a file of the sources, `build:` one its build generated (in `build/` here).\n.{{\n"
         f"    .libraries = .{{\n"]
used_build = set()
for spec in ARCHIVES:
    path, _, name = spec.rpartition("=") if "=" in spec else (spec, "", "")
    archive = os.path.realpath(os.path.join(BUILD, path))
    name = name or os.path.basename(archive).removeprefix("lib").rsplit(".", 1)[0]
    if archive not in members:
        raise SystemExit(f"no archive {archive} in the log; archives: {sorted(members)}")
    # The log holds what the archive does: as many objects as the archive on disk.
    on_disk = [m for m in subprocess.run(["ar", "t", archive], capture_output=True, text=True, check=True).stdout.split()
               if m.endswith((".o", ".obj", ".lo"))]
    if len(on_disk) != len(members[archive]):
        raise SystemExit(f"{name}: the log names {len(members[archive])} objects of {archive}, which holds {len(on_disk)}")
    groups = collections.OrderedDict()
    for o in dict.fromkeys(members[archive]):
        if o not in objects:
            # An object moved after it was compiled (gsm's Makefile compiles where it runs, then moves it): the one
            # compiled object of its name.
            same = [x for x in objects if os.path.basename(x) == os.path.basename(o)]
            if len(same) != 1:
                raise SystemExit(f"{name}: {o} was not compiled by a logged tool")
            o = same[0]
        kind, flags, includes, path_flags, source = objects[o]
        groups.setdefault((kind, flags, includes, path_flags), []).append(source)
    lines.append(f"        .{{\n            .name = \"{name}\",\n            .groups = .{{\n")
    for (kind, flags, includes, path_flags), files in groups.items():
        for p in includes + path_flags + tuple(files):
            if p.startswith("build:"):
                used_build.add(p)
        lines.append(f"                .{{\n                    .kind = .{kind},\n"
                     f"                    .flags = {zon_strings(flags, 20)},\n"
                     f"                    .include = {zon_strings(includes, 20)},\n"
                     f"                    .path_flags = {zon_strings(path_flags, 20)},\n"
                     f"                    .files = {zon_strings(files, 20)},\n                }},\n")
    lines.append("            },\n        },\n")
    print(f"{name}: {len(dict.fromkeys(members[archive]))} objects in {len(groups)} groups: "
          + ", ".join(f"{k[0]} {len(v)}" for k, v in groups.items()))
lines.append("    },\n}\n")
open(f"{OUT}/sources.zon", "w").write("".join(lines))

# The generated files the sources read: every header-like file under the build folder's include folders, and the
# generated sources and pre-included files themselves.
JUNK = ("CMakeFiles", "meson-private", "meson-logs", "meson-info", ".deps", ".libs", "subprojects")
copy = set()
for p in used_build:
    rel = p[len("build:"):]
    full = os.path.join(BUILD, rel)
    if os.path.isfile(full):
        copy.add(rel)
    elif os.path.isdir(full):
        for root, dirs, files in os.walk(full):
            dirs[:] = [d for d in dirs if not d.startswith(JUNK) and not d.endswith(".dir") and not d.endswith(".p")]
            for f in files:
                if f.endswith((".h", ".hpp", ".inc", ".asm", ".mac", ".def")):
                    copy.add(os.path.relpath(os.path.join(root, f), BUILD))
# The install prefix the harvest configured (the sysroot), which a configuration string may record, is given as the
# usual one: a static library is never installed there.
SYS = os.environ["SYS"]
for rel in sorted(copy):
    text = open(os.path.join(BUILD, rel), "rb").read()
    text = text.replace(SYS.encode(), b"/usr/local").replace(os.path.realpath(SYS).encode(), b"/usr/local")
    # A generator's command line in a comment (gperf's) names the folders it ran in.
    text = text.replace(BUILD.encode(), b"<build>").replace(SRC.encode(), b"<src>")
    if WORK.encode() in text or os.environ["WORK"].encode() in text:
        raise SystemExit(f"build/{rel} names the harvest's work folder")
    os.makedirs(os.path.dirname(os.path.join(OUT, "build", rel)), exist_ok=True)
    open(os.path.join(OUT, "build", rel), "wb").write(text)
print(f"  build/: {len(copy)} files, {sum(os.path.getsize(os.path.join(BUILD, r)) for r in copy)} bytes")
