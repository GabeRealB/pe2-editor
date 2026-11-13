//! binutils-mipsel: GNU binutils for `mipsel` (`ports/binutils-2.47`), which `pe2.psx` assembles, links and makes images
//! with.

const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    // `cargo xtask packages` asks for ReleaseSmall: a build by hand is a debug one unless told otherwise.
    const optimize = b.standardOptimizeOption(.{});
    const binutils = b.dependency("binutils", .{ .target = target, .optimize = optimize });
    for ([_][]const u8{ "as", "ld", "nm", "objcopy" }) |name| {
        b.getInstallStep().dependOn(&b.addInstallArtifact(binutils.artifact(name), .{ .dest_dir = .{ .override = .prefix } }).step);
    }
}
