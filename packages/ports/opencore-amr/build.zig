//! libopencore-amrnb and libopencore-amrwb, OpenCORE's AMR codecs, as static libraries built from what its own build compiled for each host (`hosts/`, harvested; see
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
    const src = b.dependency("opencore_amr", .{}).path("");
    const libs = harvested.libraries(b, .{
        .target = target,
        .optimize = optimize,
        .src = src,
        .host_dir = b.path(b.fmt("hosts/{t}", .{host})),
        .sources = sources.get(host),
        .links = &.{},
    });
    libs[0].installHeader(src.path(b, "amrnb/interf_dec.h"), "opencore-amrnb/interf_dec.h");
    libs[0].installHeader(src.path(b, "amrnb/interf_enc.h"), "opencore-amrnb/interf_enc.h");
    libs[1].installHeader(src.path(b, "amrwb/dec_if.h"), "opencore-amrwb/dec_if.h");
    libs[1].installHeader(src.path(b, "amrwb/if_rom.h"), "opencore-amrwb/if_rom.h");
    for (libs) |lib| b.installArtifact(lib);
}
