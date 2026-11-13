//! GCC 2.8.1's C compiler, `cc1`, for the PlayStation (`mips-sony-psx`), built with zig for any host it targets.
//!
//! What its `configure` and `Makefile` do, done here: GNU's tarball patched as old-gcc patches it for the PlayStation
//! (its `obstack.h`, `mips.patch` and `psx.patch`, `<varargs.h>` read as `<stdarg.h>`) and by our own patches, which
//! make it right on a host whose pointers are wider than int (`patches/`); the headers `configure` writes, for the
//! machine building it and for the one it is built for; the generator programs (`gen*`, `bi-*`) built for the machine
//! building it and run on the machine description, whose output is the same whatever runs them; then `cc1`.

const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    const host = b.graph.host;
    const gcc = b.dependency("gcc", .{});
    const old_gcc = b.dependency("old_gcc", .{});

    // The source, as it is built.
    const tools = b.dependency("tools", .{});
    const patching = b.addRunArtifact(tools.artifact("patch"));
    patching.addDirectoryArg(gcc.path(""));
    const source = patching.addOutputDirectoryArg("gcc-2.8.1");
    patching.addArg("replace:include <varargs.h>=include <stdarg.h>");
    patching.addPrefixedFileArg("file:obstack.h=", old_gcc.path("patches/obstack-2.8.1.h.patch"));
    patching.addPrefixedFileArg("file:config/mips/mips.h=", old_gcc.path("patches/mips.patch"));
    patching.addPrefixedFileArg("tree:", old_gcc.path("patches/psx.patch"));
    for ([_][]const u8{ "lp64-declarations", "lp64-const-double", "lp64-variadic-prototypes" }) |name| {
        patching.addPrefixedFileArg("tree:", b.path(b.fmt("patches/{s}.patch", .{name})));
    }

    // The generators, for the machine building: what they write is cc1's source.
    const building = configuration(b, host.result, old_gcc);
    const early = b.addWriteFiles();
    const insn_config = generate(b, host, source, building, &.{}, "genconfig", &.{});
    _ = early.addCopyFile(insn_config, "insn-config.h");
    const generated = b.addWriteFiles();
    _ = generated.addCopyFile(insn_config, "insn-config.h");
    const generators = [_]struct { []const u8, []const u8, []const []const u8 }{
        .{ "genflags", "insn-flags.h", &.{} },
        .{ "gencodes", "insn-codes.h", &.{} },
        .{ "genattr", "insn-attr.h", &.{} },
        .{ "genattrtab", "insn-attrtab.c", &.{ "print-rtl.c", "rtlanal.c" } },
        .{ "genemit", "insn-emit.c", &.{} },
        .{ "genopinit", "insn-opinit.c", &.{} },
        .{ "genrecog", "insn-recog.c", &.{} },
        .{ "genextract", "insn-extract.c", &.{} },
        .{ "genpeep", "insn-peep.c", &.{} },
        .{ "genoutput", "insn-output.c", &.{} },
    };
    for (generators) |g| {
        const out = generate(b, host, source, building, &.{early.getDirectory()}, g[0], g[2]);
        _ = generated.addCopyFile(out, g[1]);
    }
    for ([_][2][]const u8{ .{ "bi-arity", "bc-arity.h" }, .{ "bi-opcode", "bc-opcode.h" }, .{ "bi-opname", "bc-opname.h" } }) |g| {
        _ = generated.addCopyFile(bytecode(b, host, source, building, g[0]), g[1]);
    }

    // cc1, for the machine it is built for.
    const built = configuration(b, target.result, old_gcc);
    const cc1 = b.addExecutable(.{
        .name = "cc1",
        .root_module = b.createModule(.{
            .target = target,
            .optimize = optimize,
            .link_libc = true,
            .strip = true,
            .sanitize_c = .off,
        }),
    });
    for ([_]std.Build.LazyPath{ generated.getDirectory(), built, source, source.path(b, "config") }) |dir| {
        cc1.root_module.addIncludePath(dir);
    }
    cc1.root_module.addCSourceFiles(.{ .root = source, .files = &cc1_sources, .flags = &flags });
    cc1.root_module.addCSourceFile(.{
        .file = source.path(b, "toplev.c"),
        .flags = &(flags ++ [_][]const u8{"-DTARGET_NAME=\"mips-sony-psx\""}),
    });
    cc1.root_module.addCSourceFiles(.{ .root = generated.getDirectory(), .files = &generated_sources, .flags = &flags });
    b.installArtifact(cc1);
    // GCC's license, which goes wherever cc1 goes.
}

/// How GCC 2.8.1 is compiled: as C of its time, its overflows wrapping and its pointers aliasing as they did then.
const flags = [_][]const u8{
    "-std=gnu89",
    "-fcommon",
    "-fwrapv",
    "-fno-strict-aliasing",
    "-w",
    "-Wno-int-conversion",
    "-Wno-implicit-function-declaration",
    "-Wno-implicit-int",
    "-Wno-return-mismatch",
    "-Wno-incompatible-pointer-types",
    "-Wno-incompatible-function-pointer-types",
    "-DCROSS_COMPILE",
    "-DIN_GCC",
    "-DHAVE_CONFIG_H",
};

/// cc1's sources in GCC's tree, as its Makefile links them for this target (`toplev.c` apart, which is told the
/// target's name).
const cc1_sources = [_][]const u8{
    "c-parse.c",    "c-lang.c",    "c-lex.c",       "c-pragma.c",  "c-decl.c",    "c-typeck.c",    "c-convert.c",
    "c-aux-info.c", "c-common.c",  "c-iterate.c",   "version.c",   "tree.c",      "print-tree.c",  "stor-layout.c",
    "fold-const.c", "function.c",  "stmt.c",        "except.c",    "expr.c",      "calls.c",       "expmed.c",
    "explow.c",     "optabs.c",    "varasm.c",      "rtl.c",       "print-rtl.c", "rtlanal.c",     "emit-rtl.c",
    "real.c",       "dbxout.c",    "sdbout.c",      "dwarfout.c",  "dwarf2out.c", "xcoffout.c",    "bitmap.c",
    "integrate.c",  "jump.c",      "cse.c",         "loop.c",      "unroll.c",    "flow.c",        "stupid.c",
    "combine.c",    "regclass.c",  "local-alloc.c", "global.c",    "reload.c",    "reload1.c",     "caller-save.c",
    "reorg.c",      "sched.c",     "final.c",       "recog.c",     "reg-stack.c", "profile.c",     "config/mips/mips.c",
    "getpwd.c",     "convert.c",   "bc-emit.c",     "bc-optab.c",  "obstack.c",
};

/// The sources the generators write.
const generated_sources = [_][]const u8{
    "insn-peep.c",   "insn-opinit.c", "insn-recog.c", "insn-extract.c",
    "insn-output.c", "insn-emit.c",   "insn-attrtab.c",
};

/// The headers GCC's `configure` writes for a machine running GCC: what its C library has (`auto-config.h`), the
/// machine's description (`config.h`, and `hconfig.h` for the generators), the target's (`tm.h`, `tconfig.h`), and
/// the languages' options (`options.h`).
fn configuration(b: *std.Build, machine: std.Target, old_gcc: *std.Build.Dependency) std.Build.LazyPath {
    const files = b.addWriteFiles();
    const windows = machine.os.tag == .windows;
    if (!windows and machine.ptrBitWidth() != 64) {
        std.debug.panic("GCC 2.8.1 is built here for 64-bit hosts and Windows, not {s}", .{@tagName(machine.cpu.arch)});
    }
    _ = files.add("auto-config.h", if (windows) auto_config_windows else auto_config_posix);
    // On a 64-bit host, old-gcc's description of macOS, which holds for any whose pointers and longs are 64 bits;
    // GCC's own for Windows, built for 32 bits.
    const machine_headers = if (windows)
        "#include \"i386/xm-i386.h\"\n#include \"i386/xm-mingw32.h\"\n"
    else
        "#define NO_STAB_H\n#include \"xm-darwin.h\"\n";
    if (!windows) _ = files.addCopyFile(old_gcc.path("patches/xm-darwin.h"), "xm-darwin.h");
    _ = files.add("config.h", b.fmt("#include \"auto-config.h\"\n{s}", .{machine_headers}));
    _ = files.add("hconfig.h", machine_headers);
    _ = files.add("tm.h", "#define TARGET_CPU_DEFAULT 16\n#include \"mips/psx.h\"\n");
    _ = files.add("tconfig.h", "#include \"mips/xm-psx.h\"\n");
    // The options of the languages configured beside C, which cc1 accepts and ignores: C++'s, as `configure` enables.
    _ = files.add("options.h", "#include \"cp/lang-options.h\"\n");
    return files.getDirectory();
}

const auto_config_posix =
    \\#define HAVE_INTTYPES_H 1
    \\#define STDC_HEADERS 1
    \\#define TIME_WITH_SYS_TIME 1
    \\#define HAVE_BCMP 1
    \\#define HAVE_BCOPY 1
    \\#define HAVE_BZERO 1
    \\#define HAVE_GETRLIMIT 1
    \\#define HAVE_INDEX 1
    \\#define HAVE_KILL 1
    \\#define HAVE_POPEN 1
    \\#define HAVE_PUTENV 1
    \\#define HAVE_RINDEX 1
    \\#define HAVE_SETRLIMIT 1
    \\#define HAVE_STRERROR 1
    \\#define HAVE_SYSCONF 1
    \\#define HAVE_VPRINTF 1
    \\#define HAVE_FCNTL_H 1
    \\#define HAVE_LIMITS_H 1
    \\#define HAVE_STDDEF_H 1
    \\#define HAVE_STDLIB_H 1
    \\#define HAVE_STRING_H 1
    \\#define HAVE_STRINGS_H 1
    \\#define HAVE_SYS_FILE_H 1
    \\#define HAVE_SYS_PARAM_H 1
    \\#define HAVE_SYS_RESOURCE_H 1
    \\#define HAVE_SYS_TIME_H 1
    \\#define HAVE_SYS_TIMES_H 1
    \\#define HAVE_TIME_H 1
    \\#define HAVE_UNISTD_H 1
    \\
;

const auto_config_windows =
    \\#define HAVE_INTTYPES_H 1
    \\#define STDC_HEADERS 1
    \\#define HAVE_PUTENV 1
    \\#define HAVE_STRERROR 1
    \\#define HAVE_VPRINTF 1
    \\#define HAVE_FCNTL_H 1
    \\#define HAVE_LIMITS_H 1
    \\#define HAVE_STDDEF_H 1
    \\#define HAVE_STDLIB_H 1
    \\#define HAVE_STRING_H 1
    \\#define HAVE_SYS_TIME_H 1
    \\#define HAVE_TIME_H 1
    \\#define HAVE_UNISTD_H 1
    \\
;

/// A generator built for the machine building, and what it writes of the machine description.
fn generate(
    b: *std.Build,
    host: std.Build.ResolvedTarget,
    source: std.Build.LazyPath,
    config: std.Build.LazyPath,
    includes: []const std.Build.LazyPath,
    name: []const u8,
    more: []const []const u8,
) std.Build.LazyPath {
    const program = hostProgram(b, host, source, config, includes, name);
    var files: std.ArrayList([]const u8) = .empty;
    files.appendSlice(b.allocator, &.{ b.fmt("{s}.c", .{name}), "rtl.c", "obstack.c" }) catch @panic("OOM");
    files.appendSlice(b.allocator, more) catch @panic("OOM");
    program.root_module.addCSourceFiles(.{ .root = source, .files = files.items, .flags = &flags });
    const run = b.addRunArtifact(program);
    run.addFileArg(source.path(b, "config/mips/mips.md"));
    run.expectExitCode(0);
    return run.captureStdOut(.{ .basename = name });
}

/// A bytecode generator built for the machine building, and what it writes of `bytecode.def`.
fn bytecode(
    b: *std.Build,
    host: std.Build.ResolvedTarget,
    source: std.Build.LazyPath,
    config: std.Build.LazyPath,
    name: []const u8,
) std.Build.LazyPath {
    const program = hostProgram(b, host, source, config, &.{}, name);
    const files = [_][]const u8{ b.fmt("{s}.c", .{name}), "bi-parser.c", "bi-lexer.c", "bi-reverse.c", "obstack.c" };
    program.root_module.addCSourceFiles(.{ .root = source, .files = &files, .flags = &flags });
    const run = b.addRunArtifact(program);
    run.setStdIn(.{ .lazy_path = source.path(b, "bytecode.def") });
    // What it says on stderr (how many opcodes there are) is no failure, and kept apart.
    run.expectExitCode(0);
    _ = run.captureStdErr(.{ .basename = b.fmt("{s}.log", .{name}) });
    return run.captureStdOut(.{ .basename = name });
}

fn hostProgram(
    b: *std.Build,
    host: std.Build.ResolvedTarget,
    source: std.Build.LazyPath,
    config: std.Build.LazyPath,
    includes: []const std.Build.LazyPath,
    name: []const u8,
) *std.Build.Step.Compile {
    const program = b.addExecutable(.{
        .name = name,
        .root_module = b.createModule(.{
            .target = host,
            .optimize = .ReleaseFast,
            .link_libc = true,
            .sanitize_c = .off,
        }),
    });
    for (includes) |dir| program.root_module.addIncludePath(dir);
    for ([_]std.Build.LazyPath{ config, source, source.path(b, "config") }) |dir| {
        program.root_module.addIncludePath(dir);
    }
    return program;
}
