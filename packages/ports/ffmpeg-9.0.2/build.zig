//! FFmpeg 9.0.2's programs, `ffmpeg` and `ffprobe`, built with zig for Linux (glibc 2.28, so that what FFmpeg loads at
//! run time it can), macOS and Windows, with the libraries ported beside it.
//!
//! What FFmpeg's `configure` and `Makefile` do, done here. What configure wrote for a host - its configuration
//! headers and the lists of components - is in `hosts/<host>/config/`; which sources the host compiles and the
//! programs link, as FFmpeg's own build did, in `hosts/<host>/sources.zon`. Both were harvested by configuring FFmpeg
//! for each host with zig's C compiler against the ported libraries and making it. Every C file is compiled alike, the
//! libraries' with `HAVE_AV_CONFIG_H`; x86's assembly by nasm, run on the machine building; arm64's by clang. What the
//! build generates is generated here: swscale's x86 macros (preprocessed from a header), its arm64 kernels (written
//! by `ops_asmgen`, a program of FFmpeg's built for the machine building), and the web page and stylesheet `ffmpeg`
//! prints graphs with (`bin2c`, FFmpeg's too, on the stylesheet made one line by `minify.zig`).

const std = @import("std");

const Host = enum { linux, macos, windows };

/// A host's `sources.zon`: what FFmpeg's build compiled and linked there.
const Sources = struct {
    /// What every C file is compiled with, and where its sources find what the host lacks (`src:<folder>`).
    flags: []const []const u8,
    include: []const []const u8,
    /// What arm64's assembly is compiled with.
    assembly_flags: []const []const u8,
    library: []const []const u8,
    nasm: []const []const u8,
    assembly: []const []const u8,
    generated: []const []const u8,
    tools: []const []const u8,
    ffmpeg: []const []const u8,
    ffprobe: []const []const u8,
    system_libraries: []const []const u8,
};

const sources = std.enums.EnumArray(Host, Sources).init(.{
    .linux = @import("hosts/linux/sources.zon"),
    .macos = @import("hosts/macos/sources.zon"),
    .windows = @import("hosts/windows/sources.zon"),
});

/// The libraries FFmpeg links that are ported beside it: the name it links each by, the dependency (the port)
/// providing it, and that port's artifact.
const ported = [_]struct { []const u8, []const u8, []const u8 }{
    .{ "x264", "x264", "x264" },
    .{ "ogg", "ogg", "ogg" },
    .{ "vorbis", "vorbis", "vorbis" },
    .{ "vorbisenc", "vorbis", "vorbisenc" },
    .{ "opus", "opus", "opus" },
    .{ "speex", "speex", "speex" },
    .{ "theoradec", "theora", "theoradec" },
    .{ "theoraenc", "theora", "theoraenc" },
    .{ "lzma", "xz", "lzma" },
    .{ "openjp2", "openjpeg", "openjp2" },
    .{ "webp", "webp", "webp" },
    .{ "webpmux", "webp", "webpmux" },
    .{ "sharpyuv", "webp", "sharpyuv" },
    .{ "gsm", "gsm", "gsm" },
    .{ "opencore-amrnb", "opencore_amr", "opencore-amrnb" },
    .{ "opencore-amrwb", "opencore_amr", "opencore-amrwb" },
    .{ "vo-amrwbenc", "vo_amrwbenc", "vo-amrwbenc" },
    .{ "soxr", "soxr", "soxr" },
    .{ "xvidcore", "xvid", "xvidcore" },
    .{ "mp3lame", "libmp3lame", "mp3lame" },
    .{ "bz2", "bzip2", "bz" },
    .{ "xml2", "libxml2", "xml" },
    .{ "z", "zlib", "z" },
    .{ "dav1d", "dav1d", "dav1d" },
    .{ "aom", "aom", "aom" },
    .{ "SvtAv1Enc", "svt_av1", "SvtAv1Enc" },
    .{ "vpx", "vpx", "vpx" },
    .{ "openh264", "openh264", "openh264" },
    .{ "x265", "x265", "x265" },
    .{ "x265_main10", "x265", "x265_main10" },
    .{ "x265_main12", "x265", "x265_main12" },
    .{ "jxl", "jxl", "jxl" },
    .{ "jxl_threads", "jxl", "jxl_threads" },
    .{ "jxl_cms", "jxl", "jxl_cms" },
    .{ "hwy", "highway", "hwy" },
    .{ "brotlicommon", "brotli", "brotlicommon" },
    .{ "brotlidec", "brotli", "brotlidec" },
    .{ "brotlienc", "brotli", "brotlienc" },
    .{ "lcms2", "lcms2", "lcms2" },
    .{ "zimg", "zimg", "zimg" },
    .{ "freetype", "freetype", "freetype" },
    .{ "fribidi", "fribidi", "fribidi" },
    .{ "harfbuzz", "harfbuzz", "harfbuzz" },
    .{ "fontconfig", "fontconfig", "fontconfig" },
    .{ "expat", "libexpat", "expat" },
    .{ "ass", "libass", "ass" },
    .{ "drm", "libdrm", "drm" },
    .{ "va", "libva", "va" },
    .{ "va-drm", "libva", "va-drm" },
};

/// What FFmpeg reaches graphics cards with on Linux and Windows, which it loads when it runs: their headers alone -
/// a dependency, and the folder of it holding them.
const headers = [_]struct { []const u8, []const u8 }{
    .{ "nv_codec_headers", "include" },
    .{ "amf_headers", "" },
    .{ "vulkan_headers", "include" },
};

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    const t = target.result;
    const host: Host = switch (t.os.tag) {
        .linux => .linux,
        .macos => .macos,
        .windows => .windows,
        else => std.debug.panic("FFmpeg is built here for Linux, macOS and Windows, not {t}", .{t.os.tag}),
    };
    const x86 = switch (t.cpu.arch) {
        .x86_64 => true,
        .aarch64 => false,
        else => std.debug.panic("FFmpeg is built here for x86_64 and aarch64, not {t}", .{t.cpu.arch}),
    };
    const s = sources.get(host);
    const src = b.dependency("ffmpeg", .{}).path("");
    const config = b.path(b.fmt("hosts/{t}/config", .{host}));

    // What every C file is compiled with, as configure set it for the host; the libraries' with `HAVE_AV_CONFIG_H`.
    const program_flags = std.mem.concat(b.allocator, []const u8, &.{ s.flags, &.{"-w"} }) catch @panic("OOM");
    const library_flags = std.mem.concat(b.allocator, []const u8, &.{ program_flags, &.{"-DHAVE_AV_CONFIG_H"} }) catch @panic("OOM");
    const assembly_flags = std.mem.concat(b.allocator, []const u8, &.{ s.assembly_flags, &.{"-w"} }) catch @panic("OOM");

    // What the build generates, where the sources find it: the folder FFmpeg's build runs in.
    const generated = b.addWriteFiles();
    const host_target = b.graph.host;
    if (x86) {
        // swscale's macros for nasm, preprocessed from a header with the host's configuration.
        const cpp = b.addSystemCommand(&.{ b.graph.zig_exe, "cc", "-E", "-P", "-target" });
        cpp.addArg(t.zigTriple(b.allocator) catch @panic("OOM"));
        cpp.addArgs(library_flags);
        cpp.addPrefixedDirectoryArg("-I", config);
        cpp.addPrefixedDirectoryArg("-I", src);
        cpp.addArg("-o");
        const macros = cpp.addOutputFileArg("uops_macros.gen.asm");
        cpp.addFileArg(src.path(b, "libswscale/x86/uops_macros.asm.h"));
        _ = generated.addCopyFile(macros, "libswscale/x86/uops_macros.gen.asm");
    } else {
        // swscale's arm64 kernels and their table, written by a program of FFmpeg's.
        const asmgen = hostProgram(b, host_target, "ops_asmgen", src, &.{"libswscale/aarch64/ops_asmgen.c"}, &.{ config, src });
        for ([_][2][]const u8{ .{ "-ops", "libswscale/aarch64/ops_neon.gen.S" }, .{ "-lookup", "libswscale/aarch64/ops_lookup.gen.c" } }) |g| {
            const run = b.addRunArtifact(asmgen);
            run.addArg(g[0]);
            _ = generated.addCopyFile(run.captureStdOut(.{ .basename = std.fs.path.basename(g[1]) }), g[1]);
        }
    }
    // The page and stylesheet `ffmpeg` prints graphs with, as C arrays.
    const bin2c = hostProgram(b, host_target, "bin2c", src, &.{"ffbuild/bin2c.c"}, &.{});
    const minify = b.addExecutable(.{
        .name = "minify",
        .root_module = b.createModule(.{ .root_source_file = b.path("minify.zig"), .target = host_target, .optimize = .ReleaseFast }),
    });
    const minifying = b.addRunArtifact(minify);
    minifying.addFileArg(src.path(b, "fftools/resources/graph.css"));
    const css = minifying.addOutputFileArg("graph.css.min");
    for ([_]struct { std.Build.LazyPath, []const u8, []const u8 }{
        .{ src.path(b, "fftools/resources/graph.html"), "fftools/resources/graph.html.c", "graph_html" },
        .{ css, "fftools/resources/graph.css.c", "graph_css" },
    }) |r| {
        const run = b.addRunArtifact(bin2c);
        run.addFileArg(r[0]);
        const c = run.addOutputFileArg(std.fs.path.basename(r[1]));
        run.addArg(r[2]);
        _ = generated.addCopyFile(c, r[1]);
    }
    const build_dir = generated.getDirectory();

    // The libraries - libavutil, libavcodec, libavformat, libavfilter, libavdevice, libswscale, libswresample - as one.
    const libraries = b.addLibrary(.{
        .name = "ffmpeg",
        .linkage = .static,
        .root_module = module(b, target, optimize),
    });
    const lm = libraries.root_module;
    includes(lm, s, src, config, build_dir);
    lm.addCSourceFiles(.{ .root = src, .files = s.library, .flags = library_flags });
    lm.addCSourceFiles(.{ .root = src, .files = s.assembly, .flags = assembly_flags });
    for (s.generated) |file| {
        const flags_of = if (std.mem.endsWith(u8, file, ".S")) assembly_flags else library_flags;
        lm.addCSourceFile(.{ .file = build_dir.path(b, file), .flags = flags_of });
    }
    if (x86) {
        const nasm = b.dependency("nasm", .{ .target = host_target, .optimize = .ReleaseFast }).artifact("nasm");
        for (s.nasm) |file| {
            const run = b.addRunArtifact(nasm);
            run.addArgs(&.{ "-f", if (host == .windows) "win64" else "elf64" });
            run.addPrefixedDirectoryArg("-I", config);
            run.addPrefixedDirectoryArg("-I", build_dir);
            run.addPrefixedDirectoryArg("-I", src);
            run.addPrefixedDirectoryArg("-I", src.path(b, std.fs.path.dirname(file).?));
            run.addPrefixedFileArg("-P", config.path(b, "config.asm"));
            run.addArg("-o");
            lm.addObjectFile(run.addOutputFileArg(b.fmt("{s}.o", .{std.fs.path.stem(file)})));
            run.addFileArg(src.path(b, file));
        }
    }
    // The ported libraries the host's build linked.
    for (s.system_libraries) |lib| {
        const p = portOf(lib) orelse continue;
        // libxml2 without iconv, which macOS has only in its SDK; FFmpeg reads XML (DASH manifests) as UTF-8.
        const dependency = if (std.mem.eql(u8, p[1], "libxml2"))
            b.dependency(p[1], .{ .target = target, .optimize = optimize, .iconv = false })
        else
            b.dependency(p[1], .{ .target = target, .optimize = optimize });
        lm.linkLibrary(dependency.artifact(p[2]));
    }
    if (host != .macos) for (headers) |h| lm.addIncludePath(b.dependency(h[0], .{}).path(h[1]));

    // The programs.
    for ([_]struct { []const u8, []const []const u8 }{ .{ "ffmpeg", s.ffmpeg }, .{ "ffprobe", s.ffprobe } }) |p| {
        const exe = b.addExecutable(.{ .name = p[0], .root_module = module(b, target, optimize) });
        const m = exe.root_module;
        includes(m, s, src, config, build_dir);
        for ([_][]const []const u8{ s.tools, p[1] }) |files| for (files) |file| {
            const in_build = std.mem.startsWith(u8, file, "fftools/resources/graph.");
            m.addCSourceFile(.{ .file = if (in_build) build_dir.path(b, file) else src.path(b, file), .flags = program_flags });
        };
        m.linkLibrary(libraries);
        for (s.system_libraries) |lib| {
            if (portOf(lib) != null or std.mem.eql(u8, lib, "m") or std.mem.eql(u8, lib, "pthread")) continue;
            // C++'s library, which the C++ libraries need: zig's libc++.
            if (std.mem.eql(u8, lib, "c++")) {
                m.link_libcpp = true;
                continue;
            }
            m.linkSystemLibrary(lib, .{});
        }
        if (host == .windows) m.addWin32ResourceFile(.{ .file = src.path(b, "fftools/fftoolsres.rc") });
        b.installArtifact(exe);
    }
}

fn module(b: *std.Build, target: std.Build.ResolvedTarget, optimize: std.builtin.OptimizeMode) *std.Build.Module {
    return b.createModule(.{ .target = target, .optimize = optimize, .link_libc = true, .strip = true, .sanitize_c = .off });
}

/// Where FFmpeg's sources find what they include: the folder its build runs in (configure's files and the generated
/// ones), the sources, and the replacements it carries for what a host lacks.
fn includes(m: *std.Build.Module, s: Sources, src: std.Build.LazyPath, config: std.Build.LazyPath, build_dir: std.Build.LazyPath) void {
    const b = m.owner;
    m.addIncludePath(config);
    m.addIncludePath(build_dir);
    m.addIncludePath(src);
    for (s.include) |dir| m.addIncludePath(src.path(b, dir["src:".len..]));
}

/// A program of FFmpeg's build, for the machine building.
fn hostProgram(
    b: *std.Build,
    host: std.Build.ResolvedTarget,
    name: []const u8,
    src: std.Build.LazyPath,
    files: []const []const u8,
    include_dirs: []const std.Build.LazyPath,
) *std.Build.Step.Compile {
    const exe = b.addExecutable(.{
        .name = name,
        .root_module = b.createModule(.{ .target = host, .optimize = .ReleaseFast, .link_libc = true, .sanitize_c = .off }),
    });
    for (include_dirs) |dir| exe.root_module.addIncludePath(dir);
    exe.root_module.addCSourceFiles(.{
        .root = src,
        .files = files,
        .flags = &.{ "-D_ISOC11_SOURCE", "-D_POSIX_C_SOURCE=200112", "-D_XOPEN_SOURCE=600", "-std=c17", "-O3", "-w" },
    });
    return exe;
}

/// The port providing a library FFmpeg links, if one does.
fn portOf(lib: []const u8) ?struct { []const u8, []const u8, []const u8 } {
    for (ported) |p| if (std.mem.eql(u8, p[0], lib)) return p;
    return null;
}

