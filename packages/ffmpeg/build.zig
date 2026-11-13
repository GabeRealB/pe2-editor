//! ffmpeg: FFmpeg's programs (`ports/ffmpeg-9.0.2`), which `pe2.ffmpeg` imports and exports movies and sounds with,
//! built for speed whatever the others are built for. On Linux for glibc 2.28 rather than a static musl: what FFmpeg
//! loads at run time - the drivers of graphics cards - a static program cannot load.

const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    // Asked for as the other packages are, and not taken: FFmpeg is built for speed.
    _ = b.standardOptimizeOption(.{});
    const ffmpeg_target = if (target.result.os.tag == .linux)
        b.resolveTargetQuery(.{ .cpu_arch = target.result.cpu.arch, .os_tag = .linux, .abi = .gnu, .glibc_version = .{ .major = 2, .minor = 28, .patch = 0 } })
    else
        target;
    const ffmpeg = b.dependency("ffmpeg", .{ .target = ffmpeg_target, .optimize = .ReleaseFast });
    for ([_][]const u8{ "ffmpeg", "ffprobe" }) |name| {
        b.getInstallStep().dependOn(&b.addInstallArtifact(ffmpeg.artifact(name), .{ .dest_dir = .{ .override = .prefix } }).step);
    }
}
