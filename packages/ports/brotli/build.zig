//! libbrotlicommon, libbrotlidec and libbrotlienc, Google's Brotli compression libraries, as static libraries built from what its own build compiled for each host (`hosts/`, harvested; see
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
    const src = b.dependency("brotli", .{}).path("");
    const libs = harvested.libraries(b, .{
        .target = target,
        .optimize = optimize,
        .src = src,
        .host_dir = b.path(b.fmt("hosts/{t}", .{host})),
        .sources = sources.get(host),
        .links = &.{},
    });
    libs[0].installHeadersDirectory(src.path(b, "c/include/brotli"), "brotli", .{});
    for (libs[1..]) |lib| lib.root_module.linkLibrary(libs[0]);
    for (libs) |lib| b.installArtifact(lib);
}
