//! libogg, the Ogg container's library, as a static library: built from what its own build compiled for each host
//! (`hosts/`, harvested; see the `harvested` package).

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
    const src = b.dependency("ogg", .{}).path("");
    const host_dir = b.path(b.fmt("hosts/{t}", .{host}));
    const ogg = harvested.libraries(b, .{ .target = target, .optimize = optimize, .src = src, .host_dir = host_dir, .sources = sources.get(host) })[0];
    ogg.installHeadersDirectory(src.path(b, "include/ogg"), "ogg", .{});
    ogg.installHeadersDirectory(host_dir.path(b, "build/include/ogg"), "ogg", .{});
    b.installArtifact(ogg);
}
