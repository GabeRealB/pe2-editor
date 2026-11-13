//! libfribidi, the GNU FriBidi Unicode bidirectional algorithm library, as static libraries built from what its own build compiled for each host (`hosts/`, harvested; see
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
    const src = b.dependency("fribidi", .{}).path("");
    const host_dir = b.path(b.fmt("hosts/{t}", .{host}));
    const libs = harvested.libraries(b, .{
        .target = target,
        .optimize = optimize,
        .src = src,
        .host_dir = host_dir,
        .sources = sources.get(host),
        .links = &.{},
    });
    libs[0].installHeader(src.path(b, "lib/fribidi-arabic.h"), "fribidi-arabic.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-begindecls.h"), "fribidi-begindecls.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-bidi-types-list.h"), "fribidi-bidi-types-list.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-bidi-types.h"), "fribidi-bidi-types.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-bidi.h"), "fribidi-bidi.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-brackets.h"), "fribidi-brackets.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-char-sets-list.h"), "fribidi-char-sets-list.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-char-sets.h"), "fribidi-char-sets.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-common.h"), "fribidi-common.h");
    libs[0].installHeader(host_dir.path(b, "build/lib/fribidi-config.h"), "fribidi-config.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-deprecated.h"), "fribidi-deprecated.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-enddecls.h"), "fribidi-enddecls.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-flags.h"), "fribidi-flags.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-joining-types-list.h"), "fribidi-joining-types-list.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-joining-types.h"), "fribidi-joining-types.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-joining.h"), "fribidi-joining.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-mirroring.h"), "fribidi-mirroring.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-shape.h"), "fribidi-shape.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-types.h"), "fribidi-types.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-unicode-version.h"), "fribidi-unicode-version.h");
    libs[0].installHeader(src.path(b, "lib/fribidi-unicode.h"), "fribidi-unicode.h");
    libs[0].installHeader(src.path(b, "lib/fribidi.h"), "fribidi.h");
    for (libs[1..]) |lib| lib.root_module.linkLibrary(libs[0]);
    for (libs) |lib| b.installArtifact(lib);
}
