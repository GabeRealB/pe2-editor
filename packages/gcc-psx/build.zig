//! gcc-psx: GCC 2.8.1's cc1 for the PlayStation, which `pe2.psx` compiles packages with (`ports/gcc-2.8.1`). On Windows
//! it is built for 32 bits, as GCC knew Windows, and runs on every Windows that runs 32-bit programs.

const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    // `cargo xtask packages` asks for ReleaseSmall: a build by hand is a debug one unless told otherwise.
    const optimize = b.standardOptimizeOption(.{});
    const gcc_target = if (target.result.os.tag == .windows)
        b.resolveTargetQuery(.{ .cpu_arch = .x86, .os_tag = .windows, .abi = .gnu })
    else
        target;
    const gcc = b.dependency("gcc", .{ .target = gcc_target, .optimize = optimize });
    b.getInstallStep().dependOn(&b.addInstallArtifact(gcc.artifact("cc1"), .{ .dest_dir = .{ .override = .prefix } }).step);
}
