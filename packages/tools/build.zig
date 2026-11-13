//! The programs the ports' builds run on the machine building them, for what zig does not do itself: `patch`, which
//! applies a port's patches to its source, and `sh`, a small POSIX shell with sed, which runs the scripts a source
//! generates files with. Built for the host, whatever the packages are built for.

const std = @import("std");

pub fn build(b: *std.Build) void {
    const host = b.graph.host;
    for ([_][]const u8{ "patch", "sh" }) |name| {
        const program = b.addExecutable(.{
            .name = name,
            .root_module = b.createModule(.{
                .root_source_file = b.path(b.fmt("{s}.zig", .{name})),
                .target = host,
                .optimize = .ReleaseSafe,
                // Without debug information, which names the sources by where they are: a step running the program
                // is hashed with it, and would be named after where the project is.
                .strip = true,
            }),
        });
        b.installArtifact(program);
    }
}
