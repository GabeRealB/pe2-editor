//! libfontconfig, the Fontconfig font configuration library, as static libraries built from what its own build compiled for each host (`hosts/`, harvested; see
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
    const src = b.dependency("fontconfig", .{}).path("");
    const host_dir = b.path(b.fmt("hosts/{t}", .{host}));
    const freetype = b.dependency("freetype", .{ .target = target, .optimize = optimize }).artifact("freetype");
    const expat = b.dependency("libexpat", .{ .target = target, .optimize = optimize }).artifact("expat");
    const libs = harvested.libraries(b, .{
        .target = target,
        .optimize = optimize,
        .src = src,
        .host_dir = host_dir,
        .sources = sources.get(host),
        .links = &.{freetype, expat},
    });
    libs[0].installHeader(host_dir.path(b, "build/fontconfig/fontconfig.h"), "fontconfig/fontconfig.h");
    libs[0].installHeader(src.path(b, "fontconfig/fcfreetype.h"), "fontconfig/fcfreetype.h");
    libs[0].installHeader(src.path(b, "fontconfig/fcprivate.h"), "fontconfig/fcprivate.h");
    for (libs[1..]) |lib| lib.root_module.linkLibrary(libs[0]);
    for (libs) |lib| b.installArtifact(lib);
}
