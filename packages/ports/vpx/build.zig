//! libvpx, the WebM project's VP8 and VP9 codec library, as static libraries built from what its own build compiled for each host (`hosts/`, harvested; see
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
    const src = b.dependency("vpx", .{}).path("");
    const libs = harvested.libraries(b, .{
        .target = target,
        .optimize = optimize,
        .src = src,
        .host_dir = b.path(b.fmt("hosts/{t}", .{host})),
        .sources = sources.get(host),
        .links = &.{},
    });
    libs[0].installHeader(src.path(b, "vpx/vp8.h"), "vpx/vp8.h");
    libs[0].installHeader(src.path(b, "vpx/vp8cx.h"), "vpx/vp8cx.h");
    libs[0].installHeader(src.path(b, "vpx/vp8dx.h"), "vpx/vp8dx.h");
    libs[0].installHeader(src.path(b, "vpx/vpx_codec.h"), "vpx/vpx_codec.h");
    libs[0].installHeader(src.path(b, "vpx/vpx_decoder.h"), "vpx/vpx_decoder.h");
    libs[0].installHeader(src.path(b, "vpx/vpx_encoder.h"), "vpx/vpx_encoder.h");
    libs[0].installHeader(src.path(b, "vpx/vpx_ext_ratectrl.h"), "vpx/vpx_ext_ratectrl.h");
    libs[0].installHeader(src.path(b, "vpx/vpx_frame_buffer.h"), "vpx/vpx_frame_buffer.h");
    libs[0].installHeader(src.path(b, "vpx/vpx_image.h"), "vpx/vpx_image.h");
    libs[0].installHeader(src.path(b, "vpx/vpx_integer.h"), "vpx/vpx_integer.h");
    libs[0].installHeader(src.path(b, "vpx/vpx_tpl.h"), "vpx/vpx_tpl.h");
    for (libs[1..]) |lib| lib.root_module.linkLibrary(libs[0]);
    for (libs) |lib| b.installArtifact(lib);
}
