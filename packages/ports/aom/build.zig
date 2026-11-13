//! libaom, the Alliance for Open Media's AV1 codec library, as static libraries built from what its own build compiled for each host (`hosts/`, harvested; see
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
    const src = b.dependency("aom", .{}).path("");
    const libs = harvested.libraries(b, .{
        .target = target,
        .optimize = optimize,
        .src = src,
        .host_dir = b.path(b.fmt("hosts/{t}", .{host})),
        .sources = sources.get(host),
        .links = &.{},
    });
    libs[0].installHeader(src.path(b, "aom/aom.h"), "aom/aom.h");
    libs[0].installHeader(src.path(b, "aom/aom_codec.h"), "aom/aom_codec.h");
    libs[0].installHeader(src.path(b, "aom/aom_decoder.h"), "aom/aom_decoder.h");
    libs[0].installHeader(src.path(b, "aom/aom_encoder.h"), "aom/aom_encoder.h");
    libs[0].installHeader(src.path(b, "aom/aom_ext_ratectrl.h"), "aom/aom_ext_ratectrl.h");
    libs[0].installHeader(src.path(b, "aom/aom_external_partition.h"), "aom/aom_external_partition.h");
    libs[0].installHeader(src.path(b, "aom/aom_frame_buffer.h"), "aom/aom_frame_buffer.h");
    libs[0].installHeader(src.path(b, "aom/aom_image.h"), "aom/aom_image.h");
    libs[0].installHeader(src.path(b, "aom/aom_integer.h"), "aom/aom_integer.h");
    libs[0].installHeader(src.path(b, "aom/aom_tpl.h"), "aom/aom_tpl.h");
    libs[0].installHeader(src.path(b, "aom/aomcx.h"), "aom/aomcx.h");
    libs[0].installHeader(src.path(b, "aom/aomdx.h"), "aom/aomdx.h");
    for (libs[1..]) |lib| lib.root_module.linkLibrary(libs[0]);
    for (libs) |lib| b.installArtifact(lib);
}
