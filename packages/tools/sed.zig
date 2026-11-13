//! sed, as `sh.zig` runs it: `sed [-n] [-e <script>]... [-f <file>]... [<script>] [<file>...]`.
//!
//! Commands `s`, `d`, `p`, `h`, `H`, `g`, `G`, `x`, `n`, `q`, `t`, `b`, `:label` and `{ ... }`, each after an
//! address or a range of two (a line, `$`, `/regex/`); `s` with the flags `g`, `p` and a number, its replacement
//! holding `&`, `\1` to `\9` and escaped newlines. Regular expressions are POSIX basic ones: literals, `.`, bracket
//! expressions, `*`, `\{m,n\}`, `\(...\)`, back-references, `^` and `$`, matched leftmost and as long as backtracking
//! finds them.

const std = @import("std");
const Io = std.Io;
const Allocator = std.mem.Allocator;

pub const Error = error{ Script, Unreadable, OutOfMemory };

/// Whether sed run with these arguments reads its standard input: whether they name no file.
pub fn readsInput(args: []const []const u8) bool {
    var have_script = false;
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const a = args[i];
        if (std.mem.eql(u8, a, "-n")) continue;
        if (std.mem.eql(u8, a, "-e") or std.mem.eql(u8, a, "-f")) {
            i += 1;
            have_script = true;
        } else if (!have_script) {
            have_script = true;
        } else return false;
    }
    return true;
}

/// sed run over `stdin` - the process's own, read here, where null - or the files named (relative to `dir`), what it
/// writes appended to `out`: its exit status.
pub fn run(arena: Allocator, io: Io, dir: Io.Dir, args: []const []const u8, stdin: ?[]const u8, out: *std.ArrayList(u8)) Error!u8 {
    var script: std.ArrayList(u8) = .empty;
    var quiet = false;
    var files: std.ArrayList([]const u8) = .empty;
    var have_script = false;
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const a = args[i];
        if (std.mem.eql(u8, a, "-n")) {
            quiet = true;
        } else if (std.mem.eql(u8, a, "-e")) {
            i += 1;
            if (i >= args.len) return error.Script;
            try script.appendSlice(arena, args[i]);
            try script.append(arena, '\n');
            have_script = true;
        } else if (std.mem.eql(u8, a, "-f")) {
            i += 1;
            if (i >= args.len) return error.Script;
            const bytes = dir.readFileAlloc(io, args[i], arena, .limited(1 << 20)) catch return error.Unreadable;
            try script.appendSlice(arena, bytes);
            try script.append(arena, '\n');
            have_script = true;
        } else if (!have_script) {
            try script.appendSlice(arena, a);
            try script.append(arena, '\n');
            have_script = true;
        } else try files.append(arena, a);
    }
    var input: std.ArrayList(u8) = .empty;
    if (files.items.len == 0) {
        if (stdin) |bytes| {
            try input.appendSlice(arena, bytes);
        } else {
            var reader: Io.File.Reader = .init(Io.File.stdin(), io, &.{});
            try input.appendSlice(arena, reader.interface.allocRemaining(arena, .unlimited) catch return error.Unreadable);
        }
    } else for (files.items) |f| {
        const bytes = dir.readFileAlloc(io, f, arena, .limited(256 << 20)) catch return error.Unreadable;
        try input.appendSlice(arena, bytes);
        if (bytes.len > 0 and bytes[bytes.len - 1] != '\n') try input.append(arena, '\n');
    }
    var parser = ScriptParser{ .arena = arena, .src = script.items };
    const commands = try parser.parse();
    var machine = Machine{ .arena = arena, .commands = commands, .quiet = quiet, .out = out };
    try machine.resolveLabels();
    try machine.execute(input.items);
    return 0;
}

// ---------------------------------------------------------------------------------------------------------------
// Regular expressions.

const Atom = union(enum) {
    char: u8,
    any,
    set: struct { bits: [32]u8, negate: bool },
    group: struct { index: u8, body: []const Piece },
    backref: u8,
    bol,
    eol,
};

const Piece = struct { atom: Atom, min: usize = 1, max: ?usize = 1 };

pub const Regex = struct {
    pieces: []const Piece,
    groups: u8,
};

fn parseRegex(arena: Allocator, src: []const u8) Error!Regex {
    var p = RegexParser{ .arena = arena, .src = src };
    const pieces = try p.sequence(false);
    if (p.pos != src.len) return error.Script;
    return .{ .pieces = pieces, .groups = p.groups };
}

const RegexParser = struct {
    arena: Allocator,
    src: []const u8,
    pos: usize = 0,
    groups: u8 = 0,

    fn sequence(p: *RegexParser, in_group: bool) Error![]const Piece {
        var pieces: std.ArrayList(Piece) = .empty;
        while (p.pos < p.src.len) {
            const c = p.src[p.pos];
            if (in_group and c == '\\' and p.pos + 1 < p.src.len and p.src[p.pos + 1] == ')') break;
            var atom: Atom = undefined;
            if (c == '^' and pieces.items.len == 0) {
                atom = .bol;
                p.pos += 1;
                try pieces.append(p.arena, .{ .atom = atom });
                continue;
            }
            if (c == '$' and (p.pos + 1 == p.src.len or (in_group and std.mem.startsWith(u8, p.src[p.pos + 1 ..], "\\)")))) {
                p.pos += 1;
                try pieces.append(p.arena, .{ .atom = .eol });
                continue;
            }
            if (c == '*' and pieces.items.len == 0) {
                // A `*` with nothing before it is itself.
                atom = .{ .char = '*' };
                p.pos += 1;
            } else if (c == '.') {
                atom = .any;
                p.pos += 1;
            } else if (c == '[') {
                atom = try p.bracket();
            } else if (c == '\\') {
                if (p.pos + 1 >= p.src.len) return error.Script;
                const n = p.src[p.pos + 1];
                p.pos += 2;
                if (n == '(') {
                    p.groups += 1;
                    const index = p.groups;
                    const body = try p.sequence(true);
                    if (!std.mem.startsWith(u8, p.src[p.pos..], "\\)")) return error.Script;
                    p.pos += 2;
                    atom = .{ .group = .{ .index = index, .body = body } };
                } else if (n >= '1' and n <= '9') {
                    atom = .{ .backref = n - '0' };
                } else if (n == 'n') {
                    atom = .{ .char = '\n' };
                } else if (n == 't') {
                    atom = .{ .char = '\t' };
                } else {
                    atom = .{ .char = n };
                }
            } else {
                atom = .{ .char = c };
                p.pos += 1;
            }
            var piece = Piece{ .atom = atom };
            if (p.pos < p.src.len and p.src[p.pos] == '*') {
                piece.min = 0;
                piece.max = null;
                p.pos += 1;
            } else if (std.mem.startsWith(u8, p.src[p.pos..], "\\{")) {
                p.pos += 2;
                const close = std.mem.indexOfPos(u8, p.src, p.pos, "\\}") orelse return error.Script;
                const spec = p.src[p.pos..close];
                p.pos = close + 2;
                if (std.mem.indexOfScalar(u8, spec, ',')) |comma| {
                    piece.min = std.fmt.parseInt(usize, spec[0..comma], 10) catch return error.Script;
                    piece.max = if (comma + 1 == spec.len) null else std.fmt.parseInt(usize, spec[comma + 1 ..], 10) catch return error.Script;
                } else {
                    piece.min = std.fmt.parseInt(usize, spec, 10) catch return error.Script;
                    piece.max = piece.min;
                }
            }
            try pieces.append(p.arena, piece);
        }
        return pieces.items;
    }

    fn bracket(p: *RegexParser) Error!Atom {
        p.pos += 1;
        var bits: [32]u8 = @splat(0);
        var negate = false;
        if (p.pos < p.src.len and p.src[p.pos] == '^') {
            negate = true;
            p.pos += 1;
        }
        var first = true;
        while (p.pos < p.src.len and (first or p.src[p.pos] != ']')) : (first = false) {
            if (std.mem.startsWith(u8, p.src[p.pos..], "[:")) {
                const close = std.mem.indexOfPos(u8, p.src, p.pos + 2, ":]") orelse return error.Script;
                const class = p.src[p.pos + 2 .. close];
                p.pos = close + 2;
                for (0..256) |ch| {
                    const b: u8 = @intCast(ch);
                    const in = if (std.mem.eql(u8, class, "space"))
                        std.ascii.isWhitespace(b)
                    else if (std.mem.eql(u8, class, "blank"))
                        b == ' ' or b == '\t'
                    else if (std.mem.eql(u8, class, "digit"))
                        std.ascii.isDigit(b)
                    else if (std.mem.eql(u8, class, "alpha"))
                        std.ascii.isAlphabetic(b)
                    else if (std.mem.eql(u8, class, "alnum"))
                        std.ascii.isAlphanumeric(b)
                    else if (std.mem.eql(u8, class, "upper"))
                        std.ascii.isUpper(b)
                    else if (std.mem.eql(u8, class, "lower"))
                        std.ascii.isLower(b)
                    else
                        return error.Script;
                    if (in) bits[b / 8] |= @as(u8, 1) << @intCast(b % 8);
                }
                continue;
            }
            const lo = p.src[p.pos];
            var hi = lo;
            if (p.pos + 2 < p.src.len and p.src[p.pos + 1] == '-' and p.src[p.pos + 2] != ']') {
                hi = p.src[p.pos + 2];
                p.pos += 3;
            } else p.pos += 1;
            var ch: usize = lo;
            while (ch <= hi) : (ch += 1) bits[ch / 8] |= @as(u8, 1) << @intCast(ch % 8);
        }
        if (p.pos >= p.src.len) return error.Script;
        p.pos += 1;
        return .{ .set = .{ .bits = bits, .negate = negate } };
    }
};

const Captures = [10]?[2]usize;

/// The leftmost match of `re` in `text` at or after `start`: where it is, and its groups.
pub fn search(re: Regex, text: []const u8, start: usize, not_bol: bool) ?Captures {
    var at = start;
    while (at <= text.len) : (at += 1) {
        var caps: Captures = @splat(null);
        const m = Matcher{ .text = text, .caps = &caps, .not_bol = not_bol };
        if (m.sequence(re.pieces, null, at)) |end| {
            caps[0] = .{ at, end };
            return caps;
        }
    }
    return null;
}

/// What has to match after a group's body: the group closed where it ends, then the rest of what held it.
const Continuation = struct {
    pieces: []const Piece,
    close: ?struct { index: u8, start: usize },
    next: ?*const Continuation,
};

const Matcher = struct {
    text: []const u8,
    caps: *Captures,
    not_bol: bool,

    /// The pieces matched from `pos` on, then what continues them, as long as backtracking finds: where it ends.
    fn sequence(m: Matcher, pieces: []const Piece, then: ?*const Continuation, pos: usize) ?usize {
        if (pieces.len == 0) {
            const c = then orelse return pos;
            if (c.close) |close| {
                const saved = m.caps[close.index];
                m.caps[close.index] = .{ close.start, pos };
                if (m.sequence(c.pieces, c.next, pos)) |end| return end;
                m.caps[close.index] = saved;
                return null;
            }
            return m.sequence(c.pieces, c.next, pos);
        }
        const piece = pieces[0];
        const rest = pieces[1..];
        switch (piece.atom) {
            .bol => return if (pos == 0 and !m.not_bol) m.sequence(rest, then, pos) else null,
            .eol => return if (pos == m.text.len) m.sequence(rest, then, pos) else null,
            .group => |g| if (piece.min == 1 and piece.max != null and piece.max.? == 1) {
                // Once: its body matched with the rest of the pattern after it, so that both backtrack together.
                const after = Continuation{ .pieces = rest, .close = .{ .index = g.index, .start = pos }, .next = then };
                return m.sequence(g.body, &after, pos);
            },
            else => {},
        }
        // Repeated: as many times as it matches, then fewer until the rest does.
        var buffer: [4096]usize = undefined;
        var ends: std.ArrayList(usize) = .initBuffer(&buffer);
        ends.appendAssumeCapacity(pos);
        var at = pos;
        var count: usize = 0;
        while (piece.max == null or count < piece.max.?) {
            const next = m.atom(piece.atom, at) orelse break;
            if (next == at and count >= piece.min) break;
            at = next;
            count += 1;
            if (ends.items.len == ends.capacity) break;
            ends.appendAssumeCapacity(at);
        }
        while (ends.items.len > 0) {
            const n = ends.items.len - 1;
            if (n < piece.min) return null;
            if (m.sequence(rest, then, ends.items[n])) |end| return end;
            ends.items.len -= 1;
        }
        return null;
    }

    fn atom(m: Matcher, a: Atom, pos: usize) ?usize {
        const text = m.text;
        switch (a) {
            .char => |c| return if (pos < text.len and text[pos] == c) pos + 1 else null,
            .any => return if (pos < text.len) pos + 1 else null,
            .set => |s| {
                if (pos >= text.len) return null;
                const b = text[pos];
                const in = (s.bits[b / 8] >> @intCast(b % 8)) & 1 == 1;
                return if (in != s.negate) pos + 1 else null;
            },
            .group => |g| {
                // A repeated group: each repetition matched by itself, greedily.
                const end = m.sequence(g.body, null, pos) orelse return null;
                m.caps[g.index] = .{ pos, end };
                return end;
            },
            .backref => |n| {
                const c = m.caps[n] orelse return null;
                const sub = text[c[0]..c[1]];
                return if (std.mem.startsWith(u8, text[pos..], sub)) pos + sub.len else null;
            },
            .bol, .eol => return null,
        }
    }
};

// ---------------------------------------------------------------------------------------------------------------
// Scripts.

const Address = union(enum) { line: usize, last, regex: Regex };

const Command = struct {
    first: ?Address = null,
    second: ?Address = null,
    negate: bool = false,
    kind: u8,
    /// `s`: what is replaced, by what, and how.
    regex: ?Regex = null,
    replacement: []const u8 = "",
    global: bool = false,
    print: bool = false,
    occurrence: usize = 1,
    /// `b`, `t` and `:`: the label; `{`: where its block ends.
    label: []const u8 = "",
    target: usize = 0,
    /// A range's state: whether it is within it.
    active: bool = false,
};

const ScriptParser = struct {
    arena: Allocator,
    src: []const u8,
    pos: usize = 0,

    fn skip(p: *ScriptParser) void {
        while (p.pos < p.src.len and (p.src[p.pos] == ' ' or p.src[p.pos] == '\t' or p.src[p.pos] == '\n' or p.src[p.pos] == ';')) p.pos += 1;
    }

    fn parse(p: *ScriptParser) Error![]Command {
        var commands: std.ArrayList(Command) = .empty;
        var blocks: std.ArrayList(usize) = .empty;
        while (true) {
            p.skip();
            if (p.pos >= p.src.len) break;
            if (p.src[p.pos] == '#') {
                while (p.pos < p.src.len and p.src[p.pos] != '\n') p.pos += 1;
                continue;
            }
            var c = Command{ .kind = 0 };
            c.first = try p.address();
            if (c.first != null and p.pos < p.src.len and p.src[p.pos] == ',') {
                p.pos += 1;
                c.second = try p.address() orelse return error.Script;
            }
            while (p.pos < p.src.len and (p.src[p.pos] == ' ' or p.src[p.pos] == '\t')) p.pos += 1;
            if (p.pos < p.src.len and p.src[p.pos] == '!') {
                c.negate = true;
                p.pos += 1;
                while (p.pos < p.src.len and (p.src[p.pos] == ' ' or p.src[p.pos] == '\t')) p.pos += 1;
            }
            if (p.pos >= p.src.len) return error.Script;
            c.kind = p.src[p.pos];
            p.pos += 1;
            switch (c.kind) {
                's' => try p.substitution(&c),
                'b', 't', ':' => {
                    while (p.pos < p.src.len and (p.src[p.pos] == ' ' or p.src[p.pos] == '\t')) p.pos += 1;
                    const start = p.pos;
                    while (p.pos < p.src.len and p.src[p.pos] != '\n' and p.src[p.pos] != ';' and p.src[p.pos] != '}') p.pos += 1;
                    c.label = std.mem.trimEnd(u8, p.src[start..p.pos], " \t");
                },
                '{' => try blocks.append(p.arena, commands.items.len),
                '}' => {
                    const open = blocks.pop() orelse return error.Script;
                    commands.items[open].target = commands.items.len;
                },
                'd', 'p', 'h', 'H', 'g', 'G', 'x', 'n', 'q', '=' => {},
                else => return error.Script,
            }
            try commands.append(p.arena, c);
        }
        if (blocks.items.len != 0) return error.Script;
        return commands.items;
    }

    fn address(p: *ScriptParser) Error!?Address {
        if (p.pos >= p.src.len) return null;
        const c = p.src[p.pos];
        if (std.ascii.isDigit(c)) {
            const start = p.pos;
            while (p.pos < p.src.len and std.ascii.isDigit(p.src[p.pos])) p.pos += 1;
            return .{ .line = std.fmt.parseInt(usize, p.src[start..p.pos], 10) catch return error.Script };
        }
        if (c == '$') {
            p.pos += 1;
            return .last;
        }
        if (c == '/' or c == '\\') {
            const delimiter = if (c == '\\') blk: {
                p.pos += 1;
                break :blk p.src[p.pos];
            } else c;
            p.pos += 1;
            const text = try p.delimited(delimiter);
            return .{ .regex = try parseRegex(p.arena, text) };
        }
        return null;
    }

    /// Text up to an unescaped delimiter (consumed), the escaped delimiter itself made plain.
    fn delimited(p: *ScriptParser, delimiter: u8) Error![]const u8 {
        var out: std.ArrayList(u8) = .empty;
        var in_bracket = false;
        while (p.pos < p.src.len) {
            const c = p.src[p.pos];
            if (!in_bracket and c == delimiter) {
                p.pos += 1;
                return out.items;
            }
            if (c == '\\' and p.pos + 1 < p.src.len) {
                if (p.src[p.pos + 1] == delimiter and delimiter != '\\') {
                    try out.append(p.arena, delimiter);
                } else if (p.src[p.pos + 1] == '\n') {
                    try out.append(p.arena, '\n');
                } else {
                    try out.appendSlice(p.arena, p.src[p.pos .. p.pos + 2]);
                }
                p.pos += 2;
                continue;
            }
            if (c == '[' and !in_bracket) {
                in_bracket = true;
                try out.append(p.arena, c);
                p.pos += 1;
                // `[]...]` and `[^]...]` hold the `]`.
                if (p.pos < p.src.len and p.src[p.pos] == '^') {
                    try out.append(p.arena, '^');
                    p.pos += 1;
                }
                if (p.pos < p.src.len and p.src[p.pos] == ']') {
                    try out.append(p.arena, ']');
                    p.pos += 1;
                }
                continue;
            }
            if (c == ']' and in_bracket) in_bracket = false;
            try out.append(p.arena, c);
            p.pos += 1;
        }
        return error.Script;
    }

    fn substitution(p: *ScriptParser, c: *Command) Error!void {
        if (p.pos >= p.src.len) return error.Script;
        const delimiter = p.src[p.pos];
        p.pos += 1;
        c.regex = try parseRegex(p.arena, try p.delimited(delimiter));
        // The replacement keeps its backslashes, read when it is used; an escaped newline is a newline.
        var replacement: std.ArrayList(u8) = .empty;
        while (true) {
            if (p.pos >= p.src.len) return error.Script;
            const ch = p.src[p.pos];
            if (ch == delimiter) {
                p.pos += 1;
                break;
            }
            if (ch == '\\' and p.pos + 1 < p.src.len) {
                if (p.src[p.pos + 1] == delimiter) {
                    try replacement.append(p.arena, delimiter);
                } else if (p.src[p.pos + 1] == '\n') {
                    try replacement.append(p.arena, '\n');
                } else try replacement.appendSlice(p.arena, p.src[p.pos .. p.pos + 2]);
                p.pos += 2;
                continue;
            }
            try replacement.append(p.arena, ch);
            p.pos += 1;
        }
        c.replacement = replacement.items;
        while (p.pos < p.src.len) {
            const f = p.src[p.pos];
            if (f == 'g') {
                c.global = true;
            } else if (f == 'p') {
                c.print = true;
            } else if (std.ascii.isDigit(f)) {
                c.occurrence = f - '0';
            } else break;
            p.pos += 1;
        }
    }
};

const Machine = struct {
    arena: Allocator,
    commands: []Command,
    quiet: bool,
    out: *std.ArrayList(u8),
    hold: std.ArrayList(u8) = .empty,

    fn resolveLabels(m: *Machine) Error!void {
        for (m.commands) |*c| {
            if (c.kind != 'b' and c.kind != 't') continue;
            if (c.label.len == 0) {
                c.target = m.commands.len;
                continue;
            }
            c.target = for (m.commands, 0..) |other, i| {
                if (other.kind == ':' and std.mem.eql(u8, other.label, c.label)) break i;
            } else return error.Script;
        }
    }

    fn matches(m: *Machine, a: Address, line: usize, last: bool, space: []const u8) bool {
        _ = m;
        return switch (a) {
            .line => |n| n == line,
            .last => last,
            .regex => |re| search(re, space, 0, false) != null,
        };
    }

    fn selected(m: *Machine, c: *Command, line: usize, last: bool, space: []const u8) bool {
        const first = c.first orelse return !c.negate;
        var hit: bool = undefined;
        if (c.second) |second| {
            if (c.active) {
                hit = true;
                const ends = switch (second) {
                    .line => |n| line >= n,
                    else => m.matches(second, line, last, space),
                };
                if (ends) c.active = false;
            } else if (m.matches(first, line, last, space)) {
                hit = true;
                const ends_now = switch (second) {
                    .line => |n| n <= line,
                    else => false,
                };
                c.active = !ends_now;
            } else hit = false;
        } else hit = m.matches(first, line, last, space);
        return hit != c.negate;
    }

    fn execute(m: *Machine, input: []const u8) Error!void {
        var lines: std.ArrayList([]const u8) = .empty;
        var it = std.mem.splitScalar(u8, input, '\n');
        while (it.next()) |l| try lines.append(m.arena, l);
        // A final newline ends the last line rather than starting one more.
        const ends_in_newline = input.len > 0 and input[input.len - 1] == '\n';
        if (ends_in_newline or input.len == 0) _ = lines.pop();

        var index: usize = 0;
        while (index < lines.items.len) {
            var space: std.ArrayList(u8) = .empty;
            try space.appendSlice(m.arena, lines.items[index]);
            index += 1;
            var replaced = false;
            var pc: usize = 0;
            var deleted = false;
            var quit = false;
            while (pc < m.commands.len) {
                const c = &m.commands[pc];
                const last = index == lines.items.len;
                if (c.kind == ':' or c.kind == '}') {
                    pc += 1;
                    continue;
                }
                if (!m.selected(c, index, last, space.items)) {
                    pc = if (c.kind == '{') c.target + 1 else pc + 1;
                    continue;
                }
                switch (c.kind) {
                    '{' => {},
                    's' => {
                        if (try m.substitute(c, &space)) {
                            replaced = true;
                            if (c.print) try m.emit(space.items);
                        }
                    },
                    'd' => {
                        deleted = true;
                        break;
                    },
                    'p' => try m.emit(space.items),
                    'h' => {
                        m.hold = .empty;
                        try m.hold.appendSlice(m.arena, space.items);
                    },
                    'H' => {
                        try m.hold.append(m.arena, '\n');
                        try m.hold.appendSlice(m.arena, space.items);
                    },
                    'g' => {
                        space = .empty;
                        try space.appendSlice(m.arena, m.hold.items);
                    },
                    'G' => {
                        try space.append(m.arena, '\n');
                        try space.appendSlice(m.arena, m.hold.items);
                    },
                    'x' => {
                        const swap = space;
                        space = m.hold;
                        m.hold = swap;
                    },
                    'n' => {
                        if (!m.quiet) try m.emit(space.items);
                        if (index >= lines.items.len) {
                            deleted = true;
                            break;
                        }
                        space = .empty;
                        try space.appendSlice(m.arena, lines.items[index]);
                        index += 1;
                    },
                    'q' => {
                        quit = true;
                        break;
                    },
                    '=' => {
                        var buf: [24]u8 = undefined;
                        try m.emit(std.fmt.bufPrint(&buf, "{d}", .{index}) catch unreachable);
                    },
                    'b' => {
                        pc = c.target;
                        continue;
                    },
                    't' => if (replaced) {
                        replaced = false;
                        pc = c.target;
                        continue;
                    },
                    else => unreachable,
                }
                pc += 1;
            }
            if (!deleted and !m.quiet) try m.emit(space.items);
            if (quit) break;
        }
    }

    fn emit(m: *Machine, text: []const u8) Error!void {
        try m.out.appendSlice(m.arena, text);
        try m.out.append(m.arena, '\n');
    }

    /// `s` applied to the pattern space: whether it replaced anything.
    fn substitute(m: *Machine, c: *const Command, space: *std.ArrayList(u8)) Error!bool {
        const re = c.regex.?;
        const text = space.items;
        var out: std.ArrayList(u8) = .empty;
        var pos: usize = 0;
        var copied: usize = 0;
        var count: usize = 0;
        var any = false;
        while (pos <= text.len) {
            const caps = search(re, text, pos, false) orelse break;
            const whole = caps[0].?;
            count += 1;
            if (count >= c.occurrence) {
                try out.appendSlice(m.arena, text[copied..whole[0]]);
                try m.expandReplacement(c.replacement, text, caps, &out);
                copied = whole[1];
                any = true;
                if (!c.global) break;
            }
            // An empty match moves on by one, the character it stood before kept.
            pos = if (whole[1] == whole[0]) whole[1] + 1 else whole[1];
        }
        if (!any) return false;
        try out.appendSlice(m.arena, text[copied..]);
        space.* = out;
        return true;
    }

    fn expandReplacement(m: *Machine, replacement: []const u8, text: []const u8, caps: Captures, out: *std.ArrayList(u8)) Error!void {
        var i: usize = 0;
        while (i < replacement.len) : (i += 1) {
            const c = replacement[i];
            if (c == '&') {
                const w = caps[0].?;
                try out.appendSlice(m.arena, text[w[0]..w[1]]);
            } else if (c == '\\' and i + 1 < replacement.len) {
                i += 1;
                const n = replacement[i];
                if (n >= '0' and n <= '9') {
                    if (caps[n - '0']) |g| try out.appendSlice(m.arena, text[g[0]..g[1]]);
                } else if (n == 'n') {
                    try out.append(m.arena, '\n');
                } else if (n == 't') {
                    try out.append(m.arena, '\t');
                } else try out.append(m.arena, n);
            } else try out.append(m.arena, c);
        }
    }
};
