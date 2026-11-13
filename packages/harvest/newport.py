"""newport.py <name> <version> <url> <summary> [--links dep:artifact,...] [--headers place=dest,...]

A new harvested library's port, to harvest into: build.zig and build.zig.zon in ports/<name>, given its fingerprint and
its source fetched into the manifest, with the zig `harvest.sh` runs ($ZIG names another). A link is another port's
artifact, `<port>:<artifact>`. What it fetches then needs its entry in `components.toml`, as every source the packages
are built from does.
A header place is `src:<dir or file>` or `build:<dir or file>` (in hosts/<host>/build), installed under `dest` ("" for
the root). Its libraries are those of the host's table; the first one installs the headers and the others link it.
"""
import argparse, os, re, subprocess

PACKAGES = os.path.dirname(os.path.dirname(os.path.realpath(__file__)))
PORTS = f"{PACKAGES}/ports"
EDITOR = os.path.dirname(PACKAGES)
ZIG_VERSION = re.search(r'^\[zig\]\nversion = "([^"]+)"', open(f"{PACKAGES}/packages.toml").read(), re.M).group(1)
ZIG = os.environ.get("ZIG", f"{EDITOR}/target/packages/zig/zig-x86_64-linux-{ZIG_VERSION}/zig")
ENV = dict(os.environ, ZIG_GLOBAL_CACHE_DIR=os.environ.get("ZIG_GLOBAL_CACHE_DIR", f"{EDITOR}/target/packages/global"))
ap = argparse.ArgumentParser()
ap.add_argument("name"); ap.add_argument("version"); ap.add_argument("url"); ap.add_argument("summary")
ap.add_argument("--links", default=""); ap.add_argument("--headers", default="")
a = ap.parse_args()
# zig wants a whole semantic version.
while a.version.count(".") < 2:
    a.version += ".0"
dep = a.name.replace("-", "_")
links = [l.split(":") for l in a.links.split(",") if l]
headers = [h.split("=") for h in a.headers.split(",") if h]

link_lines = "".join(f'    const {art} = b.dependency("{d}", .{{ .target = target, .optimize = optimize }}).artifact("{art}");\n' for d, art in links)
header_lines = ""
for place, dest in headers:
    kind, path = place.split(":", 1)
    root = "src" if kind == "src" else "host_dir"
    path = path if kind == "src" else "build/" + path
    if path.endswith(".h"):
        target = f"{dest}/{os.path.basename(path)}" if dest else os.path.basename(path)
        header_lines += f'    libs[0].installHeader({root}.path(b, "{path}"), "{target}");\n'
    else:
        header_lines += f'    libs[0].installHeadersDirectory({root}.path(b, "{path}"), "{dest}", .{{}});\n'

build = f'''//! {a.summary}, as static libraries built from what its own build compiled for each host (`hosts/`, harvested; see
//! the `harvested` package).

const std = @import("std");
const harvested = @import("harvested");

const sources = std.enums.EnumArray(harvested.Host, harvested.Sources).init(.{{
    .linux = @import("hosts/linux/sources.zon"),
    .macos = @import("hosts/macos/sources.zon"),
    .windows = @import("hosts/windows/sources.zon"),
}});

pub fn build(b: *std.Build) void {{
    const target = b.standardTargetOptions(.{{}});
    const optimize = b.standardOptimizeOption(.{{}});
    const host = harvested.hostOf(target.result);
    const src = b.dependency("{dep}", .{{}}).path("");
    const host_dir = b.path(b.fmt("hosts/{{t}}", .{{host}}));
{link_lines}    const libs = harvested.libraries(b, .{{
        .target = target,
        .optimize = optimize,
        .src = src,
        .host_dir = host_dir,
        .sources = sources.get(host),
        .links = &.{{{", ".join(art for _, art in links)}}},
    }});
{header_lines}    for (libs[1..]) |lib| lib.root_module.linkLibrary(libs[0]);
    for (libs) |lib| b.installArtifact(lib);
}}
'''
if "host_dir" not in header_lines:
    build = build.replace("    const host_dir = b.path(b.fmt(\"hosts/{t}\", .{host}));\n", "").replace(".host_dir = host_dir,", '.host_dir = b.path(b.fmt("hosts/{t}", .{host})),')
deps = "".join(f'        .{d} = .{{ .path = "../{d.replace("_", "-")}" }},\n' for d, _ in links if d != "harvested")
zon = f'''.{{
    .name = .{dep},
    .version = "{a.version}",
    .minimum_zig_version = "0.17.1",
    .dependencies = .{{
        .harvested = .{{ .path = "../harvested" }},
{deps}    }},
    .paths = .{{ "build.zig", "build.zig.zon", "hosts" }},
}}
'''
d = f"{PORTS}/{a.name}"
os.makedirs(d, exist_ok=True)
open(f"{d}/build.zig", "w").write(build)
open(f"{d}/build.zig.zon", "w").write(zon)
# The fingerprint zig suggests and the source fetched, while build.zig is one that builds nothing: the port's own
# imports its tables, which the harvest has not written yet.
real = open(f"{d}/build.zig").read()
open(f"{d}/build.zig", "w").write('pub fn build(_: *@import("std").Build) void {}\n')
try:
    help_text = subprocess.run([ZIG, "build", "--help"], cwd=d, env=ENV, capture_output=True, text=True).stderr
    fingerprint = re.search(r"suggested value: (0x[0-9a-f]+)", help_text).group(1)
    zon = open(f"{d}/build.zig.zon").read().replace("    .version = ", f"    .fingerprint = {fingerprint},\n    .version = ", 1)
    open(f"{d}/build.zig.zon", "w").write(zon)
    subprocess.run([ZIG, "fetch", f"--save={dep}", a.url], cwd=d, env=ENV, check=True)
finally:
    open(f"{d}/build.zig", "w").write(real)
