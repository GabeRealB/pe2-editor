//! x264, the H.264 encoder, built with zig as a static library for FFmpeg: 8-bit and 10-bit in one, its sources
//! compiled once for each depth, with its assembly - x86's by nasm (run on the machine building), arm64's by clang.
//! What its `configure` decides of a host is in `hosts/`; which sources each host compiles, and how, as its own
//! build compiled them.

const std = @import("std");
const harvested = @import("harvested");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    const t = target.result;
    const host: []const u8 = switch (t.os.tag) {
        .linux => "linux",
        .macos => "macos",
        .windows => "windows",
        else => std.debug.panic("x264 is built here for Linux, macOS and Windows, not {t}", .{t.os.tag}),
    };
    const x86 = t.cpu.arch == .x86_64;
    if (!x86 and t.cpu.arch != .aarch64) std.debug.panic("x264 is built here for x86_64 and aarch64, not {t}", .{t.cpu.arch});
    const src = b.dependency("x264", .{}).path("");

    const config = b.addWriteFiles();
    _ = config.addCopyFile(b.path(b.fmt("hosts/{s}-config.h", .{host})), "config.h");
    const x264_config = config.addCopyFile(b.path(b.fmt("hosts/{s}-x264_config.h", .{host})), "x264_config.h");

    const lib = b.addLibrary(.{
        .name = "x264",
        .linkage = .static,
        .root_module = b.createModule(.{
            .target = target,
            .optimize = optimize,
            .link_libc = true,
            .strip = true,
            .sanitize_c = .off,
        }),
    });
    const m = lib.root_module;
    // Its per-depth sources twice, and `set.c`, `cabac.c` and `macroblock.c` both in `common/` and `encoder/`: an
    // archive holds one object of a name.
    const archives = harvested.Archives.create(b, lib);
    m.addIncludePath(config.getDirectory());
    m.addIncludePath(src);

    var cflags: std.ArrayList([]const u8) = .empty;
    cflags.appendSlice(b.allocator, &.{ "-ffast-math", "-std=gnu99", "-D_GNU_SOURCE", "-fPIC", "-fomit-frame-pointer", "-fvisibility=hidden", "-w" }) catch @panic("OOM");
    if (x86) cflags.append(b.allocator, "-mstack-alignment=64") catch @panic("OOM");
    if (t.os.tag == .windows) cflags.append(b.allocator, "-D_POSIX_C_SOURCE=200112L") catch @panic("OOM");

    addAll(archives, m, src, &shared, cflags.items);
    if (t.os.tag == .windows) addAll(archives, m, src, &.{"common/win32thread.c"}, cflags.items);

    const nasm = if (x86) b.dependency("nasm", .{ .target = b.graph.host, .optimize = .ReleaseFast }).artifact("nasm") else null;
    if (nasm) |n| m.addObjectFile(assemble(b, n, src, t, "common/x86/cpu-a.asm", &.{}));

    for ([_][]const u8{ "8", "10" }) |depth| {
        const high = if (std.mem.eql(u8, depth, "8")) "0" else "1";
        const defines = [_][]const u8{ b.fmt("-DHIGH_BIT_DEPTH={s}", .{high}), b.fmt("-DBIT_DEPTH={s}", .{depth}) };
        const flags = std.mem.concat(b.allocator, []const u8, &.{ cflags.items, &defines }) catch @panic("OOM");
        addAll(archives, m, src, &per_depth, flags);
        if (nasm) |n| {
            addAll(archives, m, src, &.{ "common/x86/mc-c.c", "common/x86/predict-c.c" }, flags);
            const asm_defines = [_][]const u8{ b.fmt("-DBIT_DEPTH={s}", .{depth}), b.fmt("-Dprivate_prefix=x264_{s}", .{depth}) };
            for (x86_asm) |file| m.addObjectFile(assemble(b, n, src, t, file, &asm_defines));
            m.addObjectFile(assemble(b, n, src, t, if (std.mem.eql(u8, depth, "8")) "common/x86/sad-a.asm" else "common/x86/sad16-a.asm", &asm_defines));
        } else {
            addAll(archives, m, src, &.{ "common/aarch64/asm-offsets.c", "common/aarch64/mc-c.c", "common/aarch64/predict-c.c" }, flags);
            const asm_flags = std.mem.concat(b.allocator, []const u8, &.{
                if (t.os.tag == .macos) &.{"-DPREFIX"} else &.{},
                &.{ "-DPIC", "-DSTACK_ALIGNMENT=16" },
                &defines,
            }) catch @panic("OOM");
            addAll(archives, m, src, &aarch64_asm, asm_flags);
        }
    }
    archives.finish();
    lib.installHeader(src.path(b, "x264.h"), "x264.h");
    lib.installHeader(x264_config, "x264_config.h");
    // With the further libraries, for whatever links what is installed.
    for (archives.libraries()) |l| b.installArtifact(l);
}

fn addAll(archives: *harvested.Archives, m: *std.Build.Module, src: std.Build.LazyPath, files: []const []const u8, flags: []const []const u8) void {
    for (files) |file| archives.addCSourceFile(m, file, .{ .file = src.path(m.owner, file), .flags = flags });
}

/// One x86 assembly file assembled by nasm, as x264's Makefile assembles it: the object made.
fn assemble(
    b: *std.Build,
    nasm: *std.Build.Step.Compile,
    src: std.Build.LazyPath,
    t: std.Target,
    file: []const u8,
    defines: []const []const u8,
) std.Build.LazyPath {
    const run = b.addRunArtifact(nasm);
    run.addPrefixedDirectoryArg("-I", src.path(b, ""));
    run.addPrefixedDirectoryArg("-I", src.path(b, "common/x86"));
    run.addArgs(&.{ "-DARCH_X86_64=1", "-f", if (t.os.tag == .windows) "win64" else "elf64", "-DSTACK_ALIGNMENT=64", "-DPIC" });
    run.addArgs(defines);
    run.addArg("-o");
    const object = run.addOutputFileArg(b.fmt("{s}{s}.o", .{ std.fs.path.stem(file), if (defines.len > 0) defines[0]["-DBIT_DEPTH=".len - 1 ..] else "" }));
    run.addFileArg(src.path(b, file));
    return object;
}

const shared = [_][]const u8{ "common/osdep.c", "common/base.c", "common/cpu.c", "common/tables.c", "encoder/api.c" };

const per_depth = [_][]const u8{
    "common/mc.c",          "common/predict.c",     "common/pixel.c",       "common/macroblock.c", "common/frame.c",
    "common/dct.c",         "common/cabac.c",       "common/common.c",      "common/rectangle.c",  "common/set.c",
    "common/quant.c",       "common/deblock.c",     "common/vlc.c",         "common/mvpred.c",     "common/bitstream.c",
    "encoder/analyse.c",    "encoder/me.c",         "encoder/ratecontrol.c", "encoder/set.c",      "encoder/macroblock.c",
    "encoder/cabac.c",      "encoder/cavlc.c",      "encoder/encoder.c",    "encoder/lookahead.c", "common/threadpool.c",
};

const x86_asm = [_][]const u8{
    "common/x86/dct-64.asm",   "common/x86/trellis-64.asm", "common/x86/bitstream-a.asm", "common/x86/const-a.asm",
    "common/x86/cabac-a.asm",  "common/x86/dct-a.asm",      "common/x86/deblock-a.asm",   "common/x86/mc-a.asm",
    "common/x86/mc-a2.asm",    "common/x86/pixel-a.asm",    "common/x86/predict-a.asm",   "common/x86/quant-a.asm",
};

const aarch64_asm = [_][]const u8{
    "common/aarch64/bitstream-a.S", "common/aarch64/cabac-a.S",      "common/aarch64/dct-a.S",      "common/aarch64/deblock-a.S",
    "common/aarch64/mc-a.S",        "common/aarch64/pixel-a.S",      "common/aarch64/predict-a.S",  "common/aarch64/quant-a.S",
    "common/aarch64/dct-a-sve.S",   "common/aarch64/deblock-a-sve.S", "common/aarch64/mc-a-sve.S",  "common/aarch64/pixel-a-sve.S",
    "common/aarch64/dct-a-sve2.S",
};
