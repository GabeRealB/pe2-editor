//! python: CPython 3.14, general-purpose (`ports/cpython-3.14`), which `pe2.psx` runs maspsx with: the program in
//! `bin/`, its standard library in `lib/python3.14`, where it finds it from there.

const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    // `cargo xtask packages` asks for ReleaseSmall: a build by hand is a debug one unless told otherwise.
    const optimize = b.standardOptimizeOption(.{});
    const cpython = b.dependency("cpython", .{ .target = target, .optimize = optimize });
    b.installArtifact(cpython.artifact("python3"));
    b.installDirectory(.{ .source_dir = cpython.namedLazyPath("stdlib"), .install_dir = .lib, .install_subdir = "python3.14" });
    // Where extension modules would be, which Python looks for to know its installation whole: none, every module
    // being built into the program.
    const note = b.addWriteFiles().add("README", "Every module of this Python is built into its program: none is loaded from here.\n");
    b.getInstallStep().dependOn(&b.addInstallFileWithDir(note, .lib, "python3.14/lib-dynload/README").step);
}
