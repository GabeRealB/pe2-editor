//! libspeex, the Speex speech codec's library, as static libraries built from what its own build compiled for each host (`hosts/`, harvested; see
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
    const src = b.dependency("speex", .{}).path("");
    const host_dir = b.path(b.fmt("hosts/{t}", .{host}));
    const libs = harvested.libraries(b, .{
        .target = target,
        .optimize = optimize,
        .src = src,
        .host_dir = host_dir,
        .sources = sources.get(host),
        .links = &.{},
    });
    libs[0].installHeadersDirectory(src.path(b, "include/speex"), "speex", .{});
    libs[0].installHeader(host_dir.path(b, "build/include/speex/speex_config_types.h"), "speex/speex_config_types.h");
    for (libs[1..]) |lib| lib.root_module.linkLibrary(libs[0]);
    for (libs) |lib| b.installArtifact(lib);
}
