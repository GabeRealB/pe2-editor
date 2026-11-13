//! libx265, the HEVC encoder, its 8-bit encoder linking its 10- and 12-bit ones, as static libraries built from what its own build compiled for each host (`hosts/`, harvested; see
//! the `harvested` package).

const std = @import("std");
const harvested = @import("harvested");

const sources = std.enums.EnumArray(harvested.Host, harvested.Sources).init(.{
    .linux = @import("hosts/linux/sources.zon"),
    .macos = @import("hosts/macos/sources.zon"),
    .windows = @import("hosts/windows/sources.zon"),
});

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    const host = harvested.hostOf(target.result);
    const src = b.dependency("x265", .{}).path("");
    const host_dir = b.path(b.fmt("hosts/{t}", .{host}));
    const libs = harvested.libraries(b, .{
        .target = target,
        .optimize = optimize,
        .src = src,
        .host_dir = host_dir,
        .sources = sources.get(host),
        .links = &.{},
    });
    libs[0].installHeader(src.path(b, "source/x265.h"), "x265.h");
    libs[0].installHeader(host_dir.path(b, "build/8bit/x265_config.h"), "x265_config.h");
    // The 8-bit encoder's C API hands 10- and 12-bit work to the others.
    for (libs[1..]) |lib| libs[0].root_module.linkLibrary(lib);
    for (libs) |lib| b.installArtifact(lib);
}
