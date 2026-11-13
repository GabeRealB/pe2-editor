//! CPython 3.14, a general-purpose Python, built with zig for any host it targets: every module of its standard
//! library that needs nothing but what CPython carries is built in - the program is one file, its standard library the
//! `.py` files beside it (`lib/python3.14`, as Python finds it from `bin/`).
//!
//! What CPython's `configure` and `Makefile` do, done here. What configure decides of a host is in `hosts/`, its
//! answers for Linux, macOS and Windows; which sources a host compiles, and how, is in the tables beside them, as configure's
//! own build compiled them for each (`hosts/sources.zon`, `hosts/modules.zon`). Windows builds from MSYS2's CPython for MinGW, which CPython itself does not build
//! for; Linux and macOS from CPython's release. The frozen modules - what Python imports before it can read a file -
//! are frozen by `_freeze_module`, built for the machine building from the same core. Left out, as no host here can
//! build them without what CPython does not carry: ssl, sqlite, ctypes, bz2, lzma, zstd, readline, curses, tkinter, dbm,
//! and remote debugging; and Apple's system log and proxy settings, whose headers are Apple's SDK's.

const std = @import("std");

const version = "3.14";
/// What Windows' resource file numbers the release by: micro * 1000 + release level * 10 + serial (3.14.10, final).
const field3 = "10150";

const Host = enum { linux, macos, windows };
const On = enum { all, posix, linux, macos, windows, @"linux+windows", @"macos+windows" };
const Group = enum {
    core,
    module,
    io,
    multiprocessing,
    hacl,
    hacl_module,
    expat,
    expat_module,
    mpdec,
    mpdec_module,
    platform,
    buildinfo,
};
const Source = struct { path: []const u8, group: Group, on: On };

/// Every source of the program but its entry, its module table and the few files given defines of their own, as the
/// reference builds compiled them (`hosts/sources.zon`, harvested).
const sources: []const Source = @import("hosts/sources.zon");

/// The modules built in, by the hosts building them (`hosts/modules.zon`, harvested).
const modules: struct { all: []const []const u8, linux: []const []const u8, macos: []const []const u8, windows: []const []const u8 } =
    @import("hosts/modules.zon");

fn hostOf(t: std.Target) Host {
    return switch (t.os.tag) {
        .linux => .linux,
        .macos => .macos,
        .windows => .windows,
        else => std.debug.panic("CPython is built here for Linux, macOS and Windows, not {t}", .{t.os.tag}),
    };
}

fn builds(on: On, host: Host) bool {
    return switch (on) {
        .all => true,
        .posix => host != .windows,
        .linux => host == .linux,
        .macos => host == .macos,
        .windows => host == .windows,
        .@"linux+windows" => host != .macos,
        .@"macos+windows" => host != .linux,
    };
}

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    const host = hostOf(target.result);
    const building = hostOf(b.graph.host.result);
    const upstream = b.dependency("python", .{}).path("");
    const tools = b.dependency("tools", .{});
    // Windows' CPython is MSYS2's, which builds for MinGW.
    const src = if (host == .windows) b.dependency("python_mingw", .{}).path("") else upstream;

    // `Python/` as it is compiled: on macOS with the system log left out (`patches/`).
    const python_dir = if (host == .macos) blk: {
        const patching = b.addRunArtifact(tools.artifact("patch"));
        patching.addDirectoryArg(src.path(b, "Python"));
        const dir = patching.addOutputDirectoryArg("Python");
        patching.addPrefixedFileArg("file:pylifecycle.c=", b.path("patches/macos-system-log.patch"));
        break :blk dir;
    } else src.path(b, "Python");

    // `_freeze_module`, for the machine building, from CPython's release: what it freezes is bytecode, the same
    // wherever it is made.
    const freezer_config = b.addWriteFiles();
    _ = freezer_config.addCopyFile(b.path(b.fmt("hosts/{t}.h", .{building})), "pyconfig.h");
    // On Linux built for musl, whose answers `hosts/linux.h` are, whatever C library the machine has.
    const freezer_target = if (building == .linux)
        b.resolveTargetQuery(.{ .cpu_arch = b.graph.host.result.cpu.arch, .os_tag = .linux, .abi = .musl })
    else
        b.graph.host;
    const freezer = program(b, .{
        .name = "_freeze_module",
        .target = freezer_target,
        .optimize = .ReleaseFast,
        .host = building,
        .src = upstream,
        .python_dir = upstream.path(b, "Python"),
        .config = freezer_config.getDirectory(),
        .module_table = moduleTable(b, tools, upstream, building, false),
        .frozen = false,
    });
    freezer.root_module.addCSourceFiles(.{
        .root = upstream,
        .files = &.{ "Programs/_freeze_module.c", "Modules/getpath_noop.c" },
        .flags = &(common_flags ++ [_][]const u8{"-DPy_BUILD_CORE"}),
    });

    // What the program is built with: its host's answers, and the modules frozen into it.
    const generated = b.addWriteFiles();
    _ = generated.addCopyFile(b.path(b.fmt("hosts/{t}.h", .{host})), "pyconfig.h");
    for (frozen_modules) |m| {
        const run = b.addRunArtifact(freezer);
        run.addArg(m[0]);
        run.addFileArg(src.path(b, m[1]));
        const header = run.addOutputFileArg(b.fmt("{s}.h", .{m[0]}));
        _ = generated.addCopyFile(header, b.fmt("Python/frozen_modules/{s}.h", .{m[0]}));
    }

    const python = program(b, .{
        .name = "python3",
        .target = target,
        .optimize = optimize,
        .host = host,
        .src = src,
        .python_dir = python_dir,
        .config = generated.getDirectory(),
        .module_table = moduleTable(b, tools, src, host, true),
        .frozen = true,
    });
    const entry = [_][]const u8{
        "-DPy_BUILD_CORE",
        "-DPYTHONPATH=\"\"",
        "-DPREFIX=\"/usr/local\"",
        "-DEXEC_PREFIX=\"/usr/local\"",
        "-DVERSION=\"" ++ version ++ "\"",
        // Where a build tree would be, which getpath looks for beside the program: none.
        "-DVPATH=\"\"",
        "-DPLATLIBDIR=\"lib\"",
        "-DPYTHONFRAMEWORK=\"\"",
    };
    python.root_module.addCSourceFiles(.{
        .root = src,
        .files = &.{ "Programs/python.c", "Modules/getpath.c" },
        .flags = std.mem.concat(b.allocator, []const u8, &.{ &common_flags, &entry, platformFlags(host) }) catch @panic("OOM"),
    });
    // zlib, through its port: what zipfile, gzip and shutil's archives need.
    const zlib = b.dependency("zlib", .{ .target = target, .optimize = optimize });
    python.root_module.linkLibrary(zlib.artifact("z"));
    python.root_module.addCSourceFile(.{
        .file = src.path(b, "Modules/zlibmodule.c"),
        .flags = std.mem.concat(b.allocator, []const u8, &.{
            &common_flags,
            &.{ "-DPy_BUILD_CORE_BUILTIN", "-DHAVE_ZLIB_COPY=1" },
            platformFlags(host),
        }) catch @panic("OOM"),
    });
    switch (host) {
        .windows => {
            python.root_module.addWin32ResourceFile(.{
                .file = src.path(b, "PC/python_exe.rc"),
                .flags = &.{"/dFIELD3=" ++ field3},
                .include_paths = &.{ src.path(b, "Include"), src.path(b, "PC"), generated.getDirectory() },
            });
            python.mingw_unicode_entry_point = true;
            python.stack_size = 4194304;
            for ([_][]const u8{ "version", "shlwapi", "api-ms-win-core-path-l1-1-0", "bcrypt", "ws2_32", "iphlpapi", "rpcrt4", "winmm" }) |lib| {
                python.root_module.linkSystemLibrary(lib, .{});
            }
        },
        .macos => python.stack_size = 1000000,
        .linux => {},
    }
    b.installArtifact(python);

    // The standard library, without what is only CPython's own tests and tools, and the GUI it cannot run.
    const lib = b.addRunArtifact(tools.artifact("patch"));
    lib.addDirectoryArg(src.path(b, "Lib"));
    const stdlib = lib.addOutputDirectoryArg("Lib");
    for ([_][]const u8{ "test", "idlelib", "tkinter", "turtledemo", "ensurepip" }) |dropped| {
        lib.addArg(b.fmt("drop:{s}", .{dropped}));
    }
    b.addNamedLazyPath("stdlib", stdlib);
}

const common_flags = [_][]const u8{ "-fno-strict-overflow", "-std=c11", "-fvisibility=hidden", "-DNDEBUG", "-w" };

fn platformFlags(host: Host) []const []const u8 {
    return switch (host) {
        .windows => &.{ "-D_WIN32_WINNT=0x0602", "-DMS_DLL_ID=\"" ++ version ++ "\"", "-Wno-error=incompatible-pointer-types" },
        else => &.{},
    };
}

const Options = struct {
    name: []const u8,
    target: std.Build.ResolvedTarget,
    optimize: std.builtin.OptimizeMode,
    host: Host,
    src: std.Build.LazyPath,
    /// `Python/` as patched; only the files patched are compiled from it (`pythonFile`).
    python_dir: std.Build.LazyPath,
    config: std.Build.LazyPath,
    module_table: std.Build.LazyPath,
    /// Whether it holds the frozen modules (`_freeze_module`, which makes them, does not).
    frozen: bool,
};

/// The files of `Python/` the port's patches change (`patches/`).
const patched = [_][]const u8{"pylifecycle.c"};

/// A file of `Python/`: from the patched copy if a patch changes it, else from the sources. A patched copy's folder is
/// named by a hash of where the project is - zig hashes the files a step reads by their absolute paths - and a source's
/// path reaches the program (an assertion's message), so only what has to be is compiled from there.
fn pythonFile(b: *std.Build, o: Options, name: []const u8) std.Build.LazyPath {
    for (patched) |p| if (std.mem.eql(u8, p, name)) return o.python_dir.path(b, name);
    return o.src.path(b, b.fmt("Python/{s}", .{name}));
}

/// CPython's core and its built-in modules, compiled for a host: the program, its entry still to add.
fn program(b: *std.Build, o: Options) *std.Build.Step.Compile {
    const exe = b.addExecutable(.{
        .name = o.name,
        .root_module = b.createModule(.{
            .target = o.target,
            .optimize = o.optimize,
            .link_libc = true,
            .strip = true,
            .sanitize_c = .off,
        }),
    });
    const m = exe.root_module;
    includes(b, m, o);
    const platform = platformFlags(o.host);
    const s = o.src;
    // A group with include folders of its own is compiled as a library of its own, so that its headers are found by
    // it alone (libmpdec's `io.h` is not Windows').
    var objects: std.EnumArray(Group, ?*std.Build.Step.Compile) = .initFill(null);
    for (sources) |source| {
        if (!builds(source.on, o.host)) continue;
        // BLAKE2's vector code is for x86's vector instructions, wherever the OS.
        const simd = std.mem.indexOf(u8, source.path, "_Simd") != null;
        if (simd and o.target.result.cpu.arch != .x86_64) continue;
        if (simd) {
            // Compiled for the vector instructions it is written for, which the program checks the processor has
            // before it calls it: its target is the program's with them added.
            const wide = std.mem.indexOf(u8, source.path, "Simd256") != null;
            const features: []const std.Target.x86.Feature = if (wide) &.{.avx2} else &.{ .sse, .sse2, .sse3, .sse4_1, .sse4_2 };
            var query = o.target.query;
            query.cpu_features_add = std.Target.x86.featureSet(features);
            const obj = b.addLibrary(.{
                .linkage = .static,
                .name = b.fmt("{s}-{s}", .{ o.name, std.fs.path.stem(source.path) }),
                .root_module = b.createModule(.{
                    .target = b.resolveTargetQuery(query),
                    .optimize = o.optimize,
                    .link_libc = true,
                    .strip = true,
                    .sanitize_c = .off,
                }),
            });
            for (groupFolders(.hacl)) |dir| obj.root_module.addIncludePath(s.path(b, dir));
            includes(b, obj.root_module, o);
            const defines: []const []const u8 = &.{ "-D_BSD_SOURCE", "-D_DEFAULT_SOURCE", if (wide) "-DHACL_CAN_COMPILE_VEC256" else "-DHACL_CAN_COMPILE_VEC128" };
            obj.root_module.addCSourceFile(.{
                .file = s.path(b, source.path),
                .flags = std.mem.concat(b.allocator, []const u8, &.{ &common_flags, platform, defines }) catch @panic("OOM"),
            });
            m.linkLibrary(obj);
            continue;
        }
        if (!o.frozen and std.mem.eql(u8, source.path, "Python/frozen.c")) continue;
        const extra: []const []const u8 = switch (source.group) {
            .core => &.{"-DPy_BUILD_CORE"},
            .module, .io, .multiprocessing, .expat_module => &.{"-DPy_BUILD_CORE_BUILTIN"},
            .hacl => &.{ "-D_BSD_SOURCE", "-D_DEFAULT_SOURCE" },
            .hacl_module => &.{ "-D_BSD_SOURCE", "-D_DEFAULT_SOURCE", "-DPy_BUILD_CORE_BUILTIN" },
            .expat => &.{},
            .mpdec, .mpdec_module => mpdec: {
                const config: []const []const u8 = if (o.host == .macos) &.{"-DUNIVERSAL=1"} else &.{ "-DCONFIG_64=1", "-DANSI=1", "-DHAVE_UINT128_T=1" };
                const builtin: []const []const u8 = if (source.group == .mpdec_module) &.{"-DPy_BUILD_CORE_BUILTIN"} else &.{};
                break :mpdec std.mem.concat(b.allocator, []const u8, &.{ config, builtin }) catch @panic("OOM");
            },
            .platform => &.{ "-DPy_BUILD_CORE", switch (o.host) {
                .linux => "-DPLATFORM=\"linux\"",
                .macos => "-DPLATFORM=\"darwin\"",
                .windows => "-DPLATFORM=\"win32\"",
            } },
            .buildinfo => &.{ if (o.host == .windows) "-DPy_BUILD_CORE_BUILTIN" else "-DPy_BUILD_CORE", "-DGITVERSION=\"\"", "-DGITTAG=\"\"", "-DGITBRANCH=\"\"" },
        };
        const flags = std.mem.concat(b.allocator, []const u8, &.{ &common_flags, platform, extra }) catch @panic("OOM");
        const folders = groupFolders(source.group);
        const into = if (folders.len == 0) m else (objects.get(source.group) orelse object: {
            const obj = b.addLibrary(.{
                .linkage = .static,
                .name = b.fmt("{s}-{t}", .{ o.name, source.group }),
                .root_module = b.createModule(.{
                    .target = o.target,
                    .optimize = o.optimize,
                    .link_libc = true,
                    .strip = true,
                    .sanitize_c = .off,
                }),
            });
            for (folders) |dir| obj.root_module.addIncludePath(s.path(b, dir));
            includes(b, obj.root_module, o);
            m.linkLibrary(obj);
            objects.set(source.group, obj);
            break :object obj;
        }).root_module;
        if (std.mem.startsWith(u8, source.path, "Python/")) {
            into.addCSourceFile(.{ .file = pythonFile(b, o, source.path["Python/".len..]), .flags = flags });
        } else {
            into.addCSourceFile(.{ .file = s.path(b, source.path), .flags = flags });
        }
    }
    // The few files given defines of their own, as the Makefile gives them.
    const core = std.mem.concat(b.allocator, []const u8, &.{ &common_flags, platform, &.{"-DPy_BUILD_CORE"} }) catch @panic("OOM");
    const sysmodule = std.mem.concat(b.allocator, []const u8, &.{ core, &.{"-DABIFLAGS=\"\""}, switch (o.host) {
        .macos => &.{"-DMULTIARCH=\"darwin\""},
        .windows => &.{"-DVPATH=\"\""},
        .linux => &.{},
    } }) catch @panic("OOM");
    m.addCSourceFile(.{ .file = pythonFile(b, o, "sysmodule.c"), .flags = sysmodule });
    if (o.host == .windows) {
        m.addCSourceFile(.{ .file = pythonFile(b, o, "dynload_win.c"), .flags = std.mem.concat(b.allocator, []const u8, &.{ core, &.{ "-DPYD_PLATFORM_TAG=\"mingw_x86_64_ucrt_llvm\"", "-DPY3_DLLNAME=L\"\"" } }) catch @panic("OOM") });
    } else {
        const soabi = if (o.host == .macos) "-DSOABI=\"cpython-314-darwin\"" else "-DSOABI=\"cpython-314\"";
        m.addCSourceFile(.{ .file = pythonFile(b, o, "dynload_shlib.c"), .flags = std.mem.concat(b.allocator, []const u8, &.{ core, &.{soabi} }) catch @panic("OOM") });
    }
    m.addCSourceFile(.{ .file = o.module_table, .flags = core });
    return exe;
}

/// The folders a group's headers are in, searched before the others.
fn groupFolders(group: Group) []const []const u8 {
    return switch (group) {
        .io => &.{"Modules/_io"},
        .multiprocessing => &.{"Modules/_multiprocessing"},
        .hacl, .hacl_module => &.{ "Modules/_hacl", "Modules/_hacl/include" },
        .expat, .expat_module => &.{"Modules/expat"},
        .mpdec, .mpdec_module => &.{"Modules/_decimal/libmpdec"},
        else => &.{},
    };
}

/// The folders every unit's headers are found in, as the Makefile orders them: the host's answers and what is
/// generated (`pyconfig.h`, `Python/frozen_modules/`), then CPython's.
fn includes(b: *std.Build, m: *std.Build.Module, o: Options) void {
    m.addIncludePath(o.config);
    m.addIncludePath(o.config.path(b, "Python"));
    for ([_][]const u8{ "Include/internal", "Include/internal/mimalloc", "Include" }) |dir| m.addIncludePath(o.src.path(b, dir));
    if (o.host == .windows) m.addIncludePath(o.src.path(b, "PC"));
}

/// `Modules/config.c`, the table of built-in modules, as makesetup writes it: CPython's template with each module's
/// declaration after its first marker and its entry after its second (`sh sed`, from the tools package).
fn moduleTable(b: *std.Build, tools: *std.Build.Dependency, src: std.Build.LazyPath, host: Host, with_zlib: bool) std.Build.LazyPath {
    var names: std.ArrayList([]const u8) = .empty;
    names.appendSlice(b.allocator, modules.all) catch @panic("OOM");
    names.appendSlice(b.allocator, switch (host) {
        .linux => modules.linux,
        .macos => modules.macos,
        .windows => modules.windows,
    }) catch @panic("OOM");
    if (with_zlib) names.append(b.allocator, "zlib") catch @panic("OOM");
    var declarations: std.ArrayList(u8) = .empty;
    var entries: std.ArrayList(u8) = .empty;
    for (names.items) |name| {
        declarations.print(b.allocator, "\\nextern PyObject* PyInit_{s}(void);", .{name}) catch @panic("OOM");
        entries.print(b.allocator, "\\n    {{\"{s}\", PyInit_{s}}},", .{ name, name }) catch @panic("OOM");
    }
    const run = b.addRunArtifact(tools.artifact("sh"));
    run.addArgs(&.{
        "sed",
        "-e",
        b.fmt("s|/\\* -- ADDMODULE MARKER 1 -- \\*/|&{s}|", .{declarations.items}),
        "-e",
        b.fmt("s|/\\* -- ADDMODULE MARKER 2 -- \\*/|&{s}|", .{entries.items}),
    });
    run.addFileArg(src.path(b, "Modules/config.c.in"));
    return run.captureStdOut(.{ .basename = "config.c" });
}

/// The modules frozen into the program, each by the file it is frozen from.
const frozen_modules = [_][2][]const u8{
    .{ "getpath", "Modules/getpath.py" },
    .{ "importlib._bootstrap", "Lib/importlib/_bootstrap.py" },
    .{ "importlib._bootstrap_external", "Lib/importlib/_bootstrap_external.py" },
    .{ "zipimport", "Lib/zipimport.py" },
    .{ "abc", "Lib/abc.py" },
    .{ "codecs", "Lib/codecs.py" },
    .{ "io", "Lib/io.py" },
    .{ "_collections_abc", "Lib/_collections_abc.py" },
    .{ "_sitebuiltins", "Lib/_sitebuiltins.py" },
    .{ "genericpath", "Lib/genericpath.py" },
    .{ "ntpath", "Lib/ntpath.py" },
    .{ "posixpath", "Lib/posixpath.py" },
    .{ "os", "Lib/os.py" },
    .{ "site", "Lib/site.py" },
    .{ "stat", "Lib/stat.py" },
    .{ "importlib.util", "Lib/importlib/util.py" },
    .{ "importlib.machinery", "Lib/importlib/machinery.py" },
    .{ "runpy", "Lib/runpy.py" },
    .{ "__hello__", "Lib/__hello__.py" },
    .{ "__phello__", "Lib/__phello__/__init__.py" },
    .{ "__phello__.ham", "Lib/__phello__/ham/__init__.py" },
    .{ "__phello__.ham.eggs", "Lib/__phello__/ham/eggs.py" },
    .{ "__phello__.spam", "Lib/__phello__/spam.py" },
    .{ "frozen_only", "Tools/freeze/flag.py" },
};
