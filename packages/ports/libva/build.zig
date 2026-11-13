//! libva and libva-drm, the Video Acceleration API's libraries, which load the graphics drivers', as static libraries built from what its own build compiled for each host (`hosts/`, harvested; see
//! the `harvested` package).

const std = @import("std");
const harvested = @import("harvested");

/// Linux's alone: it is what Linux's graphics drivers are reached through.
const linux: harvested.Sources = @import("hosts/linux/sources.zon");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    const host = harvested.hostOf(target.result);
    if (host != .linux) std.debug.panic("built for Linux only, not {t}", .{host});
    const src = b.dependency("libva", .{}).path("");
    const host_dir = b.path(b.fmt("hosts/{t}", .{host}));
    const drm = b.dependency("libdrm", .{ .target = target, .optimize = optimize }).artifact("drm");
    const libs = harvested.libraries(b, .{
        .target = target,
        .optimize = optimize,
        .src = src,
        .host_dir = host_dir,
        .sources = linux,
        .links = &.{drm},
    });
    libs[0].installHeader(src.path(b, "va/va.h"), "va/va.h");
    libs[0].installHeader(src.path(b, "va/va_backend.h"), "va/va_backend.h");
    libs[0].installHeader(src.path(b, "va/va_backend_prot.h"), "va/va_backend_prot.h");
    libs[0].installHeader(src.path(b, "va/va_backend_vpp.h"), "va/va_backend_vpp.h");
    libs[0].installHeader(src.path(b, "va/va_compat.h"), "va/va_compat.h");
    libs[0].installHeader(src.path(b, "va/va_dec_av1.h"), "va/va_dec_av1.h");
    libs[0].installHeader(src.path(b, "va/va_dec_hevc.h"), "va/va_dec_hevc.h");
    libs[0].installHeader(src.path(b, "va/va_dec_jpeg.h"), "va/va_dec_jpeg.h");
    libs[0].installHeader(src.path(b, "va/va_dec_vp8.h"), "va/va_dec_vp8.h");
    libs[0].installHeader(src.path(b, "va/va_dec_vp9.h"), "va/va_dec_vp9.h");
    libs[0].installHeader(src.path(b, "va/va_dec_vvc.h"), "va/va_dec_vvc.h");
    libs[0].installHeader(src.path(b, "va/drm/va_drm.h"), "va/va_drm.h");
    libs[0].installHeader(src.path(b, "va/va_drmcommon.h"), "va/va_drmcommon.h");
    libs[0].installHeader(src.path(b, "va/va_egl.h"), "va/va_egl.h");
    libs[0].installHeader(src.path(b, "va/va_enc_av1.h"), "va/va_enc_av1.h");
    libs[0].installHeader(src.path(b, "va/va_enc_h264.h"), "va/va_enc_h264.h");
    libs[0].installHeader(src.path(b, "va/va_enc_hevc.h"), "va/va_enc_hevc.h");
    libs[0].installHeader(src.path(b, "va/va_enc_jpeg.h"), "va/va_enc_jpeg.h");
    libs[0].installHeader(src.path(b, "va/va_enc_mpeg2.h"), "va/va_enc_mpeg2.h");
    libs[0].installHeader(src.path(b, "va/va_enc_vp8.h"), "va/va_enc_vp8.h");
    libs[0].installHeader(src.path(b, "va/va_enc_vp9.h"), "va/va_enc_vp9.h");
    libs[0].installHeader(src.path(b, "va/va_fei.h"), "va/va_fei.h");
    libs[0].installHeader(src.path(b, "va/va_fei_h264.h"), "va/va_fei_h264.h");
    libs[0].installHeader(src.path(b, "va/va_fei_hevc.h"), "va/va_fei_hevc.h");
    libs[0].installHeader(src.path(b, "va/va_prot.h"), "va/va_prot.h");
    libs[0].installHeader(src.path(b, "va/va_str.h"), "va/va_str.h");
    libs[0].installHeader(src.path(b, "va/va_tpi.h"), "va/va_tpi.h");
    libs[0].installHeader(host_dir.path(b, "build/va/va_version.h"), "va/va_version.h");
    libs[0].installHeader(src.path(b, "va/va_vpp.h"), "va/va_vpp.h");
    for (libs[1..]) |lib| lib.root_module.linkLibrary(libs[0]);
    for (libs) |lib| b.installArtifact(lib);
}
