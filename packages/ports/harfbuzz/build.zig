//! libharfbuzz, the HarfBuzz text shaping library, as static libraries built from what its own build compiled for each host (`hosts/`, harvested; see
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
    const src = b.dependency("harfbuzz", .{}).path("");
    const host_dir = b.path(b.fmt("hosts/{t}", .{host}));
    const freetype = b.dependency("freetype", .{ .target = target, .optimize = optimize }).artifact("freetype");
    const libs = harvested.libraries(b, .{
        .target = target,
        .optimize = optimize,
        .src = src,
        .host_dir = host_dir,
        .sources = sources.get(host),
        .links = &.{freetype},
    });
    libs[0].installHeader(src.path(b, "src/hb-aat-layout.h"), "hb-aat-layout.h");
    libs[0].installHeader(src.path(b, "src/hb-aat.h"), "hb-aat.h");
    libs[0].installHeader(src.path(b, "src/hb-blob.h"), "hb-blob.h");
    libs[0].installHeader(src.path(b, "src/hb-buffer.h"), "hb-buffer.h");
    libs[0].installHeader(src.path(b, "src/hb-common.h"), "hb-common.h");
    libs[0].installHeader(src.path(b, "src/hb-deprecated.h"), "hb-deprecated.h");
    libs[0].installHeader(src.path(b, "src/hb-draw.h"), "hb-draw.h");
    libs[0].installHeader(src.path(b, "src/hb-face.h"), "hb-face.h");
    libs[0].installHeader(host_dir.path(b, "build/src/hb-features.h"), "hb-features.h");
    libs[0].installHeader(src.path(b, "src/hb-font.h"), "hb-font.h");
    libs[0].installHeader(src.path(b, "src/hb-ft.h"), "hb-ft.h");
    libs[0].installHeader(src.path(b, "src/hb-gpu.h"), "hb-gpu.h");
    libs[0].installHeader(src.path(b, "src/hb-map.h"), "hb-map.h");
    libs[0].installHeader(src.path(b, "src/hb-ot-color.h"), "hb-ot-color.h");
    libs[0].installHeader(src.path(b, "src/hb-ot-deprecated.h"), "hb-ot-deprecated.h");
    libs[0].installHeader(src.path(b, "src/hb-ot-fetch.h"), "hb-ot-fetch.h");
    libs[0].installHeader(src.path(b, "src/hb-ot-font.h"), "hb-ot-font.h");
    libs[0].installHeader(src.path(b, "src/hb-ot-layout.h"), "hb-ot-layout.h");
    libs[0].installHeader(src.path(b, "src/hb-ot-math.h"), "hb-ot-math.h");
    libs[0].installHeader(src.path(b, "src/hb-ot-meta.h"), "hb-ot-meta.h");
    libs[0].installHeader(src.path(b, "src/hb-ot-metrics.h"), "hb-ot-metrics.h");
    libs[0].installHeader(src.path(b, "src/hb-ot-name.h"), "hb-ot-name.h");
    libs[0].installHeader(src.path(b, "src/hb-ot-shape.h"), "hb-ot-shape.h");
    libs[0].installHeader(src.path(b, "src/hb-ot-var.h"), "hb-ot-var.h");
    libs[0].installHeader(src.path(b, "src/hb-ot.h"), "hb-ot.h");
    libs[0].installHeader(src.path(b, "src/hb-paint.h"), "hb-paint.h");
    libs[0].installHeader(src.path(b, "src/hb-script-list.h"), "hb-script-list.h");
    libs[0].installHeader(src.path(b, "src/hb-set.h"), "hb-set.h");
    libs[0].installHeader(src.path(b, "src/hb-shape-plan.h"), "hb-shape-plan.h");
    libs[0].installHeader(src.path(b, "src/hb-shape.h"), "hb-shape.h");
    libs[0].installHeader(src.path(b, "src/hb-style.h"), "hb-style.h");
    libs[0].installHeader(src.path(b, "src/hb-unicode.h"), "hb-unicode.h");
    libs[0].installHeader(src.path(b, "src/hb-version.h"), "hb-version.h");
    libs[0].installHeader(src.path(b, "src/hb.h"), "hb.h");
    for (libs[1..]) |lib| lib.root_module.linkLibrary(libs[0]);
    for (libs) |lib| b.installArtifact(lib);
}
