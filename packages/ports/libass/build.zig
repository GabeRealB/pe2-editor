//! libass, the ASS/SSA subtitle renderer, as static libraries built from what its own build compiled for each host (`hosts/`, harvested; see
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
    const src = b.dependency("libass", .{}).path("");
    const host_dir = b.path(b.fmt("hosts/{t}", .{host}));
    const freetype = b.dependency("freetype", .{ .target = target, .optimize = optimize }).artifact("freetype");
    const fribidi = b.dependency("fribidi", .{ .target = target, .optimize = optimize }).artifact("fribidi");
    const harfbuzz = b.dependency("harfbuzz", .{ .target = target, .optimize = optimize }).artifact("harfbuzz");
    const fontconfig = b.dependency("fontconfig", .{ .target = target, .optimize = optimize }).artifact("fontconfig");
    const libs = harvested.libraries(b, .{
        .target = target,
        .optimize = optimize,
        .src = src,
        .host_dir = host_dir,
        .sources = sources.get(host),
        .links = &.{freetype, fribidi, harfbuzz, fontconfig},
    });
    libs[0].installHeader(host_dir.path(b, "build/libass/ass/ass.h"), "ass/ass.h");
    libs[0].installHeader(host_dir.path(b, "build/libass/ass/ass_types.h"), "ass/ass_types.h");
    for (libs[1..]) |lib| lib.root_module.linkLibrary(libs[0]);
    for (libs) |lib| b.installArtifact(lib);
}
