//! Libraries built from what their own builds compiled: a port keeps, for each host, a `sources.zon` saying which
//! sources each of its libraries compiles and how - in groups compiled alike - and the files its build generated
//! (`build/`), both harvested by making the library with its own build for that host with zig's compilers. A port's
//! `build.zig` imports this package (`@import("harvested")`) and calls `libraries`.
//!
//! A path in a table is `src:<path>`, in the library's sources, or `build:<path>`, in the generated files. x86
//! assembly is assembled by nasm, built for the machine building; other assembly by clang.

const std = @import("std");
const LazyPath = std.Build.LazyPath;

pub const Host = enum { linux, macos, windows };

pub const Kind = enum { c, cpp, assembly, nasm };

/// Sources compiled alike: their kind (the language they are compiled as), flags, include folders, and flags naming a file (`-include`, nasm's `-P`)
/// as pairs of the flag and the file.
pub const Group = struct {
    kind: Kind,
    flags: []const []const u8 = &.{},
    include: []const []const u8 = &.{},
    path_flags: []const []const u8 = &.{},
    files: []const []const u8,
};

pub const Library = struct {
    name: []const u8,
    groups: []const Group,
};

/// A host's `sources.zon`.
pub const Sources = struct {
    libraries: []const Library,
};

/// Nothing is built of this package by itself - ports call `libraries` - but nasm, for the machine building, for
/// the harvest (`-Dnasm`), which builds libraries with their own builds.
pub fn build(b: *std.Build) void {
    if (b.option(bool, "nasm", "Install nasm, for the machine building") orelse false) {
        b.installArtifact(b.dependency("nasm", .{ .target = b.graph.host, .optimize = .ReleaseFast }).artifact("nasm"));
    }
}

pub fn hostOf(target: std.Target) Host {
    return switch (target.os.tag) {
        .linux => .linux,
        .macos => .macos,
        .windows => .windows,
        else => std.debug.panic("built here for Linux, macOS and Windows, not {t}", .{target.os.tag}),
    };
}

pub const Options = struct {
    target: std.Build.ResolvedTarget,
    optimize: std.builtin.OptimizeMode,
    /// The library's sources.
    src: LazyPath,
    /// The host's folder of the port (`hosts/<host>`), holding `build/`.
    host_dir: LazyPath,
    sources: Sources,
    /// Libraries each of these links (another port's artifacts), whose headers they include.
    links: []const *std.Build.Step.Compile = &.{},
};

/// One static library for each library of the table, by its name.
pub fn libraries(b: *std.Build, options: Options) []*std.Build.Step.Compile {
    const built = b.allocator.alloc(*std.Build.Step.Compile, options.sources.libraries.len) catch @panic("OOM");
    const generated = options.host_dir.path(b, "build");
    var nasm: ?*std.Build.Step.Compile = null;
    for (options.sources.libraries, built) |library, *out| {
        const lib = b.addLibrary(.{
            .name = library.name,
            .linkage = .static,
            .root_module = b.createModule(.{
                .target = options.target,
                .optimize = options.optimize,
                .link_libc = true,
                .strip = true,
                .sanitize_c = .off,
            }),
        });
        const m = lib.root_module;
        for (options.links) |l| m.linkLibrary(l);
        var modules: std.ArrayList(Sub) = .empty;
        for (library.groups, 0..) |group, g| {
            if (group.kind == .cpp) m.link_libcpp = true;
            const place = struct {
                fn place(bb: *std.Build, src: LazyPath, gen: LazyPath, p: []const u8) LazyPath {
                    if (std.mem.startsWith(u8, p, "src:")) return if (p.len == 4) src else src.path(bb, p[4..]);
                    if (std.mem.startsWith(u8, p, "build:")) return if (p.len == 6) gen else gen.path(bb, p[6..]);
                    std.debug.panic("a path of a table is src: or build:, not {s}", .{p});
                }
            }.place;
            if (group.kind == .nasm) {
                if (nasm == null) {
                    const self = b.dependencyFromBuildZig(@This(), .{});
                    nasm = self.builder.dependency("nasm", .{ .target = b.graph.host, .optimize = .ReleaseFast }).artifact("nasm");
                }
                for (group.files) |file| {
                    const run = b.addRunArtifact(nasm.?);
                    // Without warnings, which say nothing of what is assembled (openh264's old hexadecimal syntax).
                    run.addArg("-w-all");
                    run.addArgs(group.flags);
                    for (group.include) |dir| run.addPrefixedDirectoryArg("-I", place(b, options.src, generated, dir));
                    var i: usize = 0;
                    while (i + 1 < group.path_flags.len) : (i += 2) {
                        run.addArg(group.path_flags[i]);
                        run.addFileArg(place(b, options.src, generated, group.path_flags[i + 1]));
                    }
                    run.addArg("-o");
                    lib.root_module.addObjectFile(run.addOutputFileArg(b.fmt("{d}-{s}.o", .{ g, std.fs.path.stem(file) })));
                    run.addFileArg(place(b, options.src, generated, file));
                }
                continue;
            }
            // Include folders belong to a module, so the sources of each set of them are a module of their own,
            // compiled into the library; a pre-included file is found through its folder, by its name.
            var dirs: std.ArrayList([]const u8) = .empty;
            dirs.appendSlice(b.allocator, group.include) catch @panic("OOM");
            var flags: std.ArrayList([]const u8) = .empty;
            flags.appendSlice(b.allocator, group.flags) catch @panic("OOM");
            flags.append(b.allocator, "-w") catch @panic("OOM");
            var i: usize = 0;
            while (i + 1 < group.path_flags.len) : (i += 2) {
                const file = group.path_flags[i + 1];
                const slash = std.mem.lastIndexOfScalar(u8, file, '/') orelse std.mem.indexOfScalar(u8, file, ':').?;
                const dir = file[0 .. slash + @intFromBool(file[slash] == ':')];
                if (!contains(dirs.items, dir)) dirs.append(b.allocator, dir) catch @panic("OOM");
                flags.appendSlice(b.allocator, &.{ group.path_flags[i], file[slash + 1 ..] }) catch @panic("OOM");
            }
            // What a group compiles for more than the target's baseline (`-msse4.1`, `-mavx2`), zig only does for a
            // whole compilation: those groups are libraries of their own, for the target with those features,
            // which the library links.
            var features: std.Target.Cpu.Feature.Set = .empty;
            var kept: std.ArrayList([]const u8) = .empty;
            for (flags.items) |flag| {
                if (std.mem.startsWith(u8, flag, "-mcpu=generic")) {
                    // The baseline CPU and extensions (`-mcpu=generic+v8_2a+dotprod`): the extensions.
                    var names = std.mem.splitScalar(u8, flag["-mcpu=generic".len..], '+');
                    while (names.next()) |name| {
                        if (name.len == 0) continue;
                        const feature = cpuFeature(options.target.result.cpu.arch, b.fmt("-m{s}", .{name})) orelse
                            std.debug.panic("{s}: no feature {s} for {t}", .{ library.name, name, options.target.result.cpu.arch });
                        if (!options.target.result.cpu.features.isEnabled(feature)) features.addFeature(feature);
                    }
                } else if (cpuFeature(options.target.result.cpu.arch, flag)) |feature| {
                    if (!options.target.result.cpu.features.isEnabled(feature)) features.addFeature(feature);
                } else kept.append(b.allocator, flag) catch @panic("OOM");
            }
            const gm = if (!features.isEmpty()) simd: {
                var query = options.target.query;
                query.cpu_features_add.addFeatureSet(features);
                const sub = b.addLibrary(.{
                    .name = b.fmt("{s}-{d}", .{ library.name, g }),
                    .linkage = .static,
                    .root_module = b.createModule(.{
                        .target = b.resolveTargetQuery(query),
                        .optimize = options.optimize,
                        .link_libc = true,
                        .strip = true,
                        .sanitize_c = .off,
                    }),
                });
                if (group.kind == .cpp) sub.root_module.link_libcpp = true;
                for (options.links) |l| sub.root_module.linkLibrary(l);
                for (dirs.items) |dir| sub.root_module.addIncludePath(place(b, options.src, generated, dir));
                m.linkLibrary(sub);
                break :simd sub.root_module;
            } else moduleFor(b, m, &modules, dirs.items) orelse made: {
                const module = modules.items[modules.items.len - 1].module;
                for (options.links) |l| module.linkLibrary(l);
                for (dirs.items) |dir| module.addIncludePath(place(b, options.src, generated, dir));
                break :made module;
            };
            if (group.kind == .cpp) gm.link_libcpp = true;
            // Each file in the language its group compiled it as, whatever its name says (opencore-amr's `.cpp`
            // files are C).
            const language: std.Build.Module.CSourceLanguage = switch (group.kind) {
                .c => .c,
                .cpp => .cpp,
                .assembly => .assembly_with_preprocessor,
                .nasm => unreachable,
            };
            for (group.files) |file| gm.addCSourceFile(.{ .file = place(b, options.src, generated, file), .flags = kept.items, .language = language });
        }
        out.* = lib;
    }
    return built;
}

/// A module of a library's, compiling the sources whose include folders are `dirs`.
const Sub = struct { dirs: []const []const u8, module: *std.Build.Module };

/// The module of these folders if there is one, else null and a new one, last of `modules`, given no folders yet.
fn moduleFor(b: *std.Build, root: *std.Build.Module, modules: *std.ArrayList(Sub), dirs: []const []const u8) ?*std.Build.Module {
    for (modules.items) |sub| {
        if (sub.dirs.len != dirs.len) continue;
        for (sub.dirs, dirs) |x, y| {
            if (!std.mem.eql(u8, x, y)) break;
        } else return sub.module;
    }
    const module = b.createModule(.{
        .target = root.resolved_target,
        .optimize = root.optimize,
        .link_libc = true,
        .strip = true,
        .sanitize_c = .off,
    });
    root.addImport(b.fmt("sources{d}", .{modules.items.len}), module);
    modules.append(b.allocator, .{ .dirs = b.allocator.dupe([]const u8, dirs) catch @panic("OOM"), .module = module }) catch @panic("OOM");
    return null;
}

fn contains(items: []const []const u8, item: []const u8) bool {
    for (items) |x| if (std.mem.eql(u8, x, item)) return true;
    return false;
}

/// The CPU feature a compiler flag asks for (`-msse4.1` is x86's `sse4_1`), if it is one of the arch's.
fn cpuFeature(arch: std.Target.Cpu.Arch, flag: []const u8) ?std.Target.Cpu.Feature.Set.Index {
    if (!std.mem.startsWith(u8, flag, "-m") or std.mem.startsWith(u8, flag, "-mno-")) return null;
    var name_buf: [64]u8 = undefined;
    if (flag.len - 2 > name_buf.len) return null;
    const name = name_buf[0 .. flag.len - 2];
    for (flag[2..], name) |c, *n| n.* = if (c == '.' or c == '-') '_' else c;
    return switch (arch) {
        .x86_64, .x86 => if (std.meta.stringToEnum(std.Target.x86.Feature, name)) |f| @intFromEnum(f) else null,
        .aarch64 => if (std.meta.stringToEnum(std.Target.aarch64.Feature, name)) |f| @intFromEnum(f) else null,
        else => null,
    };
}
