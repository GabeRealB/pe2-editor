//! libdrm, the Direct Rendering Manager's userspace library (its core only), as static libraries built from what its own build compiled for each host (`hosts/`, harvested; see
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
    const src = b.dependency("libdrm", .{}).path("");
    const libs = harvested.libraries(b, .{
        .target = target,
        .optimize = optimize,
        .src = src,
        .host_dir = b.path(b.fmt("hosts/{t}", .{host})),
        .sources = linux,
        .links = &.{},
    });
    libs[0].installHeader(src.path(b, "xf86drm.h"), "xf86drm.h");
    libs[0].installHeader(src.path(b, "xf86drmMode.h"), "xf86drmMode.h");
    libs[0].installHeader(src.path(b, "include/drm/amdgpu_drm.h"), "amdgpu_drm.h");
    libs[0].installHeader(src.path(b, "include/drm/drm.h"), "drm.h");
    libs[0].installHeader(src.path(b, "include/drm/drm_fourcc.h"), "drm_fourcc.h");
    libs[0].installHeader(src.path(b, "include/drm/drm_mode.h"), "drm_mode.h");
    libs[0].installHeader(src.path(b, "include/drm/drm_sarea.h"), "drm_sarea.h");
    libs[0].installHeader(src.path(b, "include/drm/i915_drm.h"), "i915_drm.h");
    libs[0].installHeader(src.path(b, "include/drm/mach64_drm.h"), "mach64_drm.h");
    libs[0].installHeader(src.path(b, "include/drm/mga_drm.h"), "mga_drm.h");
    libs[0].installHeader(src.path(b, "include/drm/msm_drm.h"), "msm_drm.h");
    libs[0].installHeader(src.path(b, "include/drm/nouveau_drm.h"), "nouveau_drm.h");
    libs[0].installHeader(src.path(b, "include/drm/qxl_drm.h"), "qxl_drm.h");
    libs[0].installHeader(src.path(b, "include/drm/r128_drm.h"), "r128_drm.h");
    libs[0].installHeader(src.path(b, "include/drm/radeon_drm.h"), "radeon_drm.h");
    libs[0].installHeader(src.path(b, "include/drm/savage_drm.h"), "savage_drm.h");
    libs[0].installHeader(src.path(b, "include/drm/sis_drm.h"), "sis_drm.h");
    libs[0].installHeader(src.path(b, "include/drm/tegra_drm.h"), "tegra_drm.h");
    libs[0].installHeader(src.path(b, "include/drm/vc4_drm.h"), "vc4_drm.h");
    libs[0].installHeader(src.path(b, "include/drm/via_drm.h"), "via_drm.h");
    libs[0].installHeader(src.path(b, "include/drm/virtgpu_drm.h"), "virtgpu_drm.h");
    for (libs[1..]) |lib| lib.root_module.linkLibrary(libs[0]);
    for (libs) |lib| b.installArtifact(lib);
}
