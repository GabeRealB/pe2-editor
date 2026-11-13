//! `cpp`, the preprocessor `pe2.psx` runs (`cpp.zig`), built of aro as the zig building it carries it: aro's source is
//! zig's (`lib/compiler/aro`), at the version this project pins.

const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    const zig_dir = std.fs.path.dirname(b.graph.zig_exe) orelse ".";
    const aro_dir: std.Build.LazyPath = .{ .cwd_relative = b.pathJoin(&.{ zig_dir, "lib", "compiler", "aro" }) };

    // What aro's own build gives it: no tracing or allocation checks, and a toolchain's defaults, which only
    // linking reads.
    const options = b.addOptions();
    options.addOption(bool, "debug_allocations", false);
    options.addOption(bool, "enable_tracy", false);
    options.addOption(bool, "enable_tracy_allocation", false);
    options.addOption(bool, "enable_tracy_callstack", false);
    const defaults = b.addOptions();
    defaults.addOption([]const u8, "linker", "ld");
    defaults.addOption([]const u8, "sysroot", "");
    defaults.addOption([]const u8, "gcc_install_prefix", "");
    defaults.addOption([]const u8, "rtlib", "platform");
    defaults.addOption([]const u8, "unwindlib", "platform");
    const aro = b.createModule(.{ .root_source_file = aro_dir.path(b, "aro.zig") });
    aro.addOptions("build_options", options);
    aro.addOptions("system_defaults", defaults);

    const cpp = b.addExecutable(.{
        .name = "cpp",
        .root_module = b.createModule(.{
            .root_source_file = b.path("cpp.zig"),
            .target = target,
            .optimize = optimize,
            .strip = true,
        }),
    });
    cpp.root_module.addImport("aro", aro);
    b.getInstallStep().dependOn(&b.addInstallArtifact(cpp, .{ .dest_dir = .{ .override = .prefix } }).step);
}
