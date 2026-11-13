//! A source tree copied with changes made to it, for builds that have to change what they fetch: zig has no `patch`.
//!
//! `patch <source> <out> <operation>...` copies the tree at `<source>` to `<out>`, the operations applied in order:
//!
//! - `tree:<patch>`: a unified diff of several files, each named by its `+++` path less its first component (`-p1`).
//! - `file:<path>=<patch>`: a unified diff applied to the one file `<path>`, whatever its headers name.
//! - `replace:<from>=<to>`: `<from>` replaced by `<to>` in every `.c` file at the tree's top.
//! - `drop:<path>`: the file or folder at `<path>` left out.
//!
//! A hunk is found by what it removes and keeps, searched outward from the line its header names, as `patch` does
//! without fuzz: one that is nowhere in its file fails the run by name. Text before a diff's first file is its
//! description, and is read past.

const std = @import("std");
const Io = std.Io;
const Allocator = std.mem.Allocator;

/// The tree, by path relative to its top (`/`-separated), with each file's bytes.
const Tree = std.StringArrayHashMapUnmanaged([]const u8);

pub fn main(init: std.process.Init) !void {
    const arena = init.arena.allocator();
    const io = init.io;
    const args = try init.minimal.args.toSlice(arena);
    if (args.len < 3) std.process.fatal("usage: patch <source> <out> <operation>...", .{});

    var tree: Tree = .empty;
    var source = try Io.Dir.cwd().openDir(io, args[1], .{ .iterate = true });
    defer source.close(io);
    var walker = try source.walk(arena);
    defer walker.deinit();
    while (try walker.next(io)) |entry| {
        if (entry.kind != .file) continue;
        const path = try arena.dupe(u8, entry.path);
        std.mem.replaceScalar(u8, path, '\\', '/');
        const bytes = try source.readFileAlloc(io, entry.path, arena, .limited(64 << 20));
        try tree.put(arena, path, bytes);
    }

    for (args[3..]) |operation| {
        if (std.mem.startsWith(u8, operation, "tree:")) {
            const diff = try Io.Dir.cwd().readFileAlloc(io, operation["tree:".len..], arena, .limited(16 << 20));
            try applyDiff(arena, &tree, diff, null, operation);
        } else if (std.mem.startsWith(u8, operation, "file:")) {
            const spec = operation["file:".len..];
            const eq = std.mem.indexOfScalar(u8, spec, '=') orelse std.process.fatal("{s}: no `=`", .{operation});
            const diff = try Io.Dir.cwd().readFileAlloc(io, spec[eq + 1 ..], arena, .limited(16 << 20));
            try applyDiff(arena, &tree, diff, spec[0..eq], operation);
        } else if (std.mem.startsWith(u8, operation, "replace:")) {
            const spec = operation["replace:".len..];
            const eq = std.mem.indexOfScalar(u8, spec, '=') orelse std.process.fatal("{s}: no `=`", .{operation});
            for (tree.keys(), tree.values()) |path, *bytes| {
                if (std.mem.indexOfScalar(u8, path, '/') != null or !std.mem.endsWith(u8, path, ".c")) continue;
                bytes.* = try std.mem.replaceOwned(u8, arena, bytes.*, spec[0..eq], spec[eq + 1 ..]);
            }
        } else if (std.mem.startsWith(u8, operation, "drop:")) {
            const path = operation["drop:".len..];
            var i: usize = 0;
            var dropped = false;
            while (i < tree.count()) {
                const key = tree.keys()[i];
                const under = std.mem.eql(u8, key, path) or
                    (std.mem.startsWith(u8, key, path) and key.len > path.len and key[path.len] == '/');
                if (under) {
                    tree.swapRemoveAt(i);
                    dropped = true;
                } else i += 1;
            }
            if (!dropped) std.process.fatal("{s}: nothing there", .{operation});
        } else std.process.fatal("no operation {s}", .{operation});
    }

    var out = try Io.Dir.cwd().createDirPathOpen(io, args[2], .{});
    defer out.close(io);
    for (tree.keys(), tree.values()) |path, bytes| {
        if (std.fs.path.dirname(path)) |dir| try out.createDirPath(io, dir);
        try out.writeFile(io, .{ .sub_path = path, .data = bytes });
    }
}

/// Every file of a unified diff applied to the tree: to `only` where it is given, otherwise to the file each names.
fn applyDiff(arena: Allocator, tree: *Tree, diff: []const u8, only: ?[]const u8, operation: []const u8) !void {
    var lines = std.mem.splitScalar(u8, diff, '\n');
    var target: ?[]const u8 = null;
    var hunks: usize = 0;
    // How far a file's lines have moved from where the diff's headers say, by the hunks applied before.
    var delta: isize = 0;
    while (lines.next()) |line| {
        if (std.mem.startsWith(u8, line, "+++ ")) {
            target = only orelse try stripFirst(arena, line["+++ ".len..]);
            delta = 0;
        } else if (std.mem.startsWith(u8, line, "@@ ")) {
            const path = target orelse std.process.fatal("{s}: a hunk before any file", .{operation});
            delta = try applyHunk(arena, tree, path, line, &lines, delta, operation);
            hunks += 1;
        }
    }
    if (hunks == 0) std.process.fatal("{s}: no hunk", .{operation});
}

/// A `+++` path less its date and first component (`gcc-2.8.1/config/mips/psx.h` is `config/mips/psx.h`).
fn stripFirst(arena: Allocator, header: []const u8) ![]const u8 {
    const end = std.mem.indexOfAny(u8, header, "\t") orelse header.len;
    const path = std.mem.trimEnd(u8, header[0..end], " \r");
    const slash = std.mem.indexOfScalar(u8, path, '/') orelse return arena.dupe(u8, path);
    return arena.dupe(u8, path[slash + 1 ..]);
}

/// One hunk applied, its lines read from `lines`: how far it moved the lines after it.
fn applyHunk(
    arena: Allocator,
    tree: *Tree,
    path: []const u8,
    header: []const u8,
    lines: *std.mem.SplitIterator(u8, .scalar),
    delta: isize,
    operation: []const u8,
) !isize {
    // `@@ -<start>[,<count>] +<start>[,<count>] @@`
    var fields = std.mem.tokenizeAny(u8, header, " @");
    const old = try range(fields.next() orelse "", '-', operation);
    const new = try range(fields.next() orelse "", '+', operation);

    var before: std.ArrayList([]const u8) = .empty;
    var after: std.ArrayList([]const u8) = .empty;
    while (before.items.len < old.count or after.items.len < new.count) {
        const line = lines.next() orelse std.process.fatal("{s}: {s}: a hunk ends early", .{ operation, path });
        if (line.len == 0) {
            // A context line of an empty line, its leading space trimmed by whoever wrote the diff.
            try before.append(arena, "");
            try after.append(arena, "");
            continue;
        }
        switch (line[0]) {
            ' ' => {
                try before.append(arena, line[1..]);
                try after.append(arena, line[1..]);
            },
            '-' => try before.append(arena, line[1..]),
            '+' => try after.append(arena, line[1..]),
            '\\' => {}, // `\ No newline at end of file`
            else => std.process.fatal("{s}: {s}: a hunk line starts with '{c}'", .{ operation, path, line[0] }),
        }
    }

    const existing = tree.get(path);
    if (existing == null and old.count != 0) std.process.fatal("{s}: no file {s}", .{ operation, path });
    var file: std.ArrayList([]const u8) = .empty;
    if (existing) |bytes| {
        var split = std.mem.splitScalar(u8, bytes, '\n');
        while (split.next()) |line| try file.append(arena, line);
    }
    // A file ending in a newline splits into a last, empty piece, which no hunk names.
    const ends_in_newline = file.items.len > 0 and file.items[file.items.len - 1].len == 0;
    if (ends_in_newline) _ = file.pop();

    const hint: isize = @as(isize, @intCast(if (old.start > 0) old.start - 1 else 0)) + delta;
    const at = find(file.items, before.items, hint) orelse
        std.process.fatal("{s}: {s}: the hunk {s} is nowhere in the file", .{ operation, path, header });
    try file.replaceRange(arena, at, before.items.len, after.items);

    var bytes: std.ArrayList(u8) = .empty;
    for (file.items, 0..) |line, i| {
        try bytes.appendSlice(arena, line);
        if (i + 1 < file.items.len or ends_in_newline or existing == null) try bytes.append(arena, '\n');
    }
    try tree.put(arena, path, bytes.items);
    return @as(isize, @intCast(at)) - (hint - delta) + @as(isize, @intCast(after.items.len)) -
        @as(isize, @intCast(before.items.len));
}

const Range = struct { start: usize, count: usize };

fn range(field: []const u8, sign: u8, operation: []const u8) !Range {
    if (field.len == 0 or field[0] != sign) std.process.fatal("{s}: a hunk header without {c}", .{ operation, sign });
    const text = field[1..];
    const comma = std.mem.indexOfScalar(u8, text, ',');
    const start = try std.fmt.parseInt(usize, if (comma) |c| text[0..c] else text, 10);
    const count = if (comma) |c| try std.fmt.parseInt(usize, text[c + 1 ..], 10) else 1;
    return .{ .start = start, .count = count };
}

/// Where `block` is in `file`, nearest `hint` first.
fn find(file: []const []const u8, block: []const []const u8, hint: isize) ?usize {
    if (block.len > file.len) return null;
    const last: isize = @intCast(file.len - block.len);
    var distance: isize = 0;
    while (distance <= last + @as(isize, @intCast(@abs(hint))) + 1) : (distance += 1) {
        for ([_]isize{ hint - distance, hint + distance }) |at| {
            if (at < 0 or at > last) continue;
            const i: usize = @intCast(at);
            if (matches(file[i .. i + block.len], block)) return i;
        }
    }
    return null;
}

fn matches(lines: []const []const u8, block: []const []const u8) bool {
    for (lines, block) |a, b| {
        if (!std.mem.eql(u8, std.mem.trimEnd(u8, a, "\r"), std.mem.trimEnd(u8, b, "\r"))) return false;
    }
    return true;
}
