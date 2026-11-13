"""cpython.py <work> <upstream source> <MinGW source> <port> <host>...: CPython's port's files, from each host's reference
build (<work>/build/cpython-<host>).

- `hosts/<host>.h`: its pyconfig.h, without comments or blank lines, under a header saying what it is.
- `hosts/sources.zon`: every source the builds compiled into the program, with the group saying how it is compiled
  (the flags it has beyond those every source shares) and the hosts compiling it; but the program's entry, its module
  table and the files the port gives defines of its own.
- `hosts/modules.zon`: the modules each build's module table (Modules/config.c) holds, those every host's first.
"""
import os, re, shlex, subprocess, sys

WORK, UPSTREAM, MINGW, PORT = (os.path.realpath(p) for p in sys.argv[1:5])
HOSTS = sys.argv[5:]
TARGETS = {"linux": "x86_64-linux-musl", "macos": "aarch64-macos", "windows": "x86_64-windows-gnu"}
WHAT = {"linux": "Linux, with musl", "macos": "macOS", "windows": "Windows, with MinGW-w64 (MSYS2's CPython)"}
# Compiled by the port with defines of its own, or not built in.
NOT_TABLED = ("Programs/python.c", "Modules/config.c", "Modules/_dbmmodule.c")


def header(host):
    text = (f"/* pyconfig.h for {WHAT[host]}: what CPython 3.14.10's configure decided for that host, run with zig's C "
            f"compiler for {TARGETS[host]}\n   (`--disable-shared --without-mimalloc --without-remote-debug --with-pkg-config=no`).")
    if host == "macos":
        text += (" openpty, login_tty,\n   forkpty and sendfile are marked absent: they link, but the C library headers zig "
                 "carries for macOS declare none.")
    return text + " */\n\n"


def objects(host):
    """Each object linked into the build's program: its source, relative to the source tree, and its flags."""
    build = f"{WORK}/build/cpython-{host}"
    src = MINGW if host == "windows" else UPSTREAM
    text = open(f"{build}/make.log").read().replace("\\\n", " ")
    compiled = {}
    for line in text.split("\n"):
        if f"cc-{TARGETS[host]}" not in line or " -c " not in line or " -o " not in line:
            continue
        try:
            words = shlex.split(line)
        except ValueError:
            continue
        out = words[words.index("-o") + 1]
        source = [w for w in words if w.endswith(".c")]
        if not source:
            continue
        flags = [w for w in words[1:] if w not in ("-c", "-o", out) and not w.endswith(".c")]
        flags = [f.replace(f"{src}/", "$SRC/").replace(src, "$SRC") for f in flags]
        compiled[out] = (source[-1].replace(f"{src}/", ""), flags)
    link = [l for l in text.split("\n") if f"cc-{TARGETS[host]}" in l and re.search(r"-o python(\.exe)? Programs/python\.o", l)][-1]
    linked = [w for w in shlex.split(link) if w.endswith(".o")]
    for archive in [w for w in shlex.split(link) if w.endswith(".a") and not w.startswith("libpython")]:
        members = subprocess.run(["ar", "t", f"{build}/{archive}"], capture_output=True, text=True, check=True).stdout.split()
        linked += [f"{os.path.dirname(archive)}/{m}" for m in members]
    entries = {o: compiled[o] for o in linked if o in compiled}
    common = None
    for _, flags in entries.values():
        common = flags if common is None else [x for x in common if x in flags]
    return {o: (s, [x for x in f if x not in common]) for o, (s, f) in entries.items()}


def group(extra):
    """How a source is compiled, by the flags it has beyond those every source shares."""
    e = [x for x in extra if not x.startswith("-fPIC")]
    has = lambda f: any(f in x for x in e)
    builtin = has("BUILTIN")
    if has("libmpdec"):
        return "mpdec_module" if builtin else "mpdec"
    if has("_hacl"):
        return "hacl_module" if builtin else "hacl"
    if has("expat"):
        return "expat_module" if builtin else "expat"
    if has("Modules/_io"):
        return "io"
    if has("_multiprocessing"):
        return "multiprocessing"
    if has("PLATFORM="):
        return "platform"
    if has("GITVERSION"):
        return "buildinfo"
    if e == ["-DPy_BUILD_CORE"]:
        return "core"
    if builtin:
        return "module"
    raise SystemExit(f"no group for the flags {' '.join(e)}")


for host in HOSTS:
    text = re.sub(r"/\*.*?\*/", "", open(f"{WORK}/build/cpython-{host}/pyconfig.h").read(), flags=re.S)
    body = "\n".join(l for l in (l.rstrip() for l in text.split("\n")) if l.strip())
    open(f"{PORT}/hosts/{host}.h", "w").write(header(host) + body + "\n")

table = {}
for host in HOSTS:
    for source, extra in objects(host).values():
        source = source.replace("Modules/../PC/", "PC/")
        if source in NOT_TABLED:
            continue
        table.setdefault(source, {})[host] = group(extra)
rows = []
for source in sorted(table):
    groups = set(table[source].values())
    # A source compiled differently by host is the build information's (Windows builds it as a module).
    g = groups.pop() if len(groups) == 1 else "buildinfo"
    if groups and g != "buildinfo":
        raise SystemExit(f"{source}: compiled as {table[source]}")
    on = sorted(table[source])
    o = "all" if len(on) == 3 else ("posix" if on == ["linux", "macos"] else on[0] if len(on) == 1 else '@"' + "+".join(on) + '"')
    rows.append(f'    .{{ .path = "{source}", .group = .{g}, .on = .{o} }},')
open(f"{PORT}/hosts/sources.zon", "w").write(
    "// Every source of the program but its entry, its module table and the few files given defines of their own: each with how it is\n"
    "// compiled and on which hosts, as the reference builds compiled it. Written by the harvest (packages/harvest/recipes/cpython.sh).\n"
    ".{\n" + "\n".join(rows) + "\n}\n")

names = {}
for host in HOSTS:
    config = open(f"{WORK}/build/cpython-{host}/Modules/config.c").read()
    body = config.split("_PyImport_Inittab[] = {")[1].split("ADDMODULE MARKER 2")[0]
    # dbm, which macOS' build finds and the port does not build.
    names[host] = [n for n in re.findall(r'\{"([A-Za-z0-9_]+)", PyInit_', body) if n != "_dbm"]
every = [n for n in names[HOSTS[0]] if all(n in names[h] for h in HOSTS)]
lines = ["// The modules built into the program, by the hosts that build them: what each reference build's module table (Modules/config.c)",
         "// holds, those of every host first. Written by the harvest (packages/harvest/recipes/cpython.sh).", ".{"]
for key, items in [("all", every)] + [(h, [n for n in names[h] if n not in every]) for h in HOSTS]:
    lines.append(f"    .{key} = .{{")
    lines += ["        " + " ".join(f'"{x}",' for x in items[i:i + 8]) for i in range(0, len(items), 8)]
    lines.append("    },")
open(f"{PORT}/hosts/modules.zon", "w").write("\n".join(lines + ["}"]) + "\n")
print(f"  hosts/{{{','.join(HOSTS)}}}.h, hosts/sources.zon ({len(rows)} sources), hosts/modules.zon "
      f"({len(every)} built in everywhere)")
