//! `minify <css> <out>`: a stylesheet made one line, as FFmpeg's build makes the one its programs embed with `sed`
//! and `tr` - on each line the stretch from its first `/*` to its last `*/` dropped, lines joined by spaces, runs of
//! spaces made one, and a space at either end dropped.

const std = @import("std");
const Io = std.Io;

pub fn main(init: std.process.Init) !void {
    const arena = init.arena.allocator();
    const io = init.io;
    const args = try init.minimal.args.toSlice(arena);
    if (args.len != 3) std.process.fatal("usage: minify <css> <out>", .{});
    const css = Io.Dir.cwd().readFileAlloc(io, args[1], arena, .limited(16 << 20)) catch |e|
        std.process.fatal("{s}: {t}", .{ args[1], e });

    var joined: std.ArrayList(u8) = .empty;
    var lines = std.mem.splitScalar(u8, css, '\n');
    while (lines.next()) |line| {
        var kept = line;
        if (std.mem.indexOf(u8, line, "/*")) |open| {
            if (std.mem.lastIndexOf(u8, line, "*/")) |close| {
                if (close >= open + 2) kept = try std.mem.concat(arena, u8, &.{ line[0..open], line[close + 2 ..] });
            }
        }
        try joined.appendSlice(arena, kept);
        if (lines.index != null) try joined.append(arena, ' ');
    }
    var squeezed: std.ArrayList(u8) = .empty;
    for (joined.items) |c| {
        if (c == ' ' and squeezed.items.len > 0 and squeezed.items[squeezed.items.len - 1] == ' ') continue;
        try squeezed.append(arena, c);
    }
    var text = squeezed.items;
    if (std.mem.startsWith(u8, text, " ")) text = text[1..];
    if (std.mem.endsWith(u8, text, " ")) text = text[0 .. text.len - 1];
    Io.Dir.cwd().writeFile(io, .{ .sub_path = args[2], .data = text }) catch |e| std.process.fatal("{s}: {t}", .{ args[2], e });
}
