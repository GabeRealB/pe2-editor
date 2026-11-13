//! libjxl, libjxl_threads and libjxl_cms, the JPEG XL reference codec's libraries, as static libraries built from what its own build compiled for each host (`hosts/`, harvested; see
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
    const src = b.dependency("jxl", .{}).path("");
    const host_dir = b.path(b.fmt("hosts/{t}", .{host}));
    const hwy = b.dependency("highway", .{ .target = target, .optimize = optimize }).artifact("hwy");
    const brotlicommon = b.dependency("brotli", .{ .target = target, .optimize = optimize }).artifact("brotlicommon");
    const brotlidec = b.dependency("brotli", .{ .target = target, .optimize = optimize }).artifact("brotlidec");
    const brotlienc = b.dependency("brotli", .{ .target = target, .optimize = optimize }).artifact("brotlienc");
    const lcms2 = b.dependency("lcms2", .{ .target = target, .optimize = optimize }).artifact("lcms2");
    const libs = harvested.libraries(b, .{
        .target = target,
        .optimize = optimize,
        .src = src,
        .host_dir = host_dir,
        .sources = sources.get(host),
        .links = &.{hwy, brotlicommon, brotlidec, brotlienc, lcms2},
    });
    libs[0].installHeadersDirectory(host_dir.path(b, "build/lib/include/jxl"), "jxl", .{});
    // libjxl manages color with libjxl_cms; libjxl_threads runs libjxl's work.
    libs[0].root_module.linkLibrary(libs[2]);
    libs[1].root_module.linkLibrary(libs[0]);
    for (libs) |lib| b.installArtifact(lib);
}
