//! A small POSIX shell, for the scripts a build has to run and zig cannot: binutils makes ld's emulations and BFD's
//! target table with `sh` and `sed`, and a build that needs nothing of its machine runs them with this.
//!
//! `sh [-C <dir>] <script> [<argument>...]` runs a script; `sh [-C <dir>] -c <command> [<name> <argument>...]` a
//! command; `sh sed ...` the sed this shell has (`sed.zig`), as a program of its own. `-C` makes `<dir>` the folder
//! relative paths are found in, as `cd` would. `.` given arguments makes them the positional parameters of what it
//! reads, as bash does. It is what those scripts use of a shell and no more: the
//! grammar - lists, `&&` and `||`, pipelines, subshells and groups, `if`, `for`, `case`, `while` and `until`,
//! functions, here-documents and redirections - parameters and their expansions (`${x-y}`, `${x:+y}`, `${#x}` and the
//! like), command substitution and field splitting, without pathname expansion or jobs; and as builtins the commands
//! those scripts run: `.`, `:`, `cat`, `cd`, `cmp`, `echo`, `eval`, `exit`, `export`, `false`, `mkdir`, `printf` (`%s`
//! only), `return`, `rm`, `sed`, `set`, `shift`, `test` and `[`, `true`, `unset`; `PWD` is the folder it is in, as an
//! absolute path, so that a build can give it relative paths and have them made absolute here, where they do not
//! reach what the build hashes. Anything else fails by name. A pipeline runs
//! its commands one after the other, each given what the one before wrote.

const std = @import("std");
const Io = std.Io;
const Allocator = std.mem.Allocator;
const sed = @import("sed.zig");

pub fn main(init: std.process.Init) !void {
    const arena = init.arena.allocator();
    const io = init.io;
    const args = try init.minimal.args.toSlice(arena);
    if (args.len < 2) std.process.fatal("usage: sh <script> [<argument>...] | sh -c <command> | sh sed ...", .{});

    if (std.mem.eql(u8, args[1], "sed")) {
        var out: std.ArrayList(u8) = .empty;
        const status = sed.run(arena, io, Io.Dir.cwd(), args[2..], null, &out) catch |e| std.process.fatal("sed: {t}", .{e});
        try Io.File.stdout().writeStreamingAll(io, out.items);
        std.process.exit(status);
    }

    var shell = Shell{ .arena = arena, .io = io, .dir = Io.Dir.cwd() };
    var a: []const []const u8 = args[1..];
    if (a.len >= 2 and std.mem.eql(u8, a[0], "-C")) {
        shell.dir = Io.Dir.cwd().openDir(io, a[1], .{}) catch |e| std.process.fatal("{s}: {t}", .{ a[1], e });
        a = a[2..];
    }
    shell.setPwd() catch |e| std.process.fatal("sh: the folder's path: {t}", .{e});
    if (a.len == 0) std.process.fatal("sh: no script", .{});
    var source: []const u8 = undefined;
    var rest: []const []const u8 = undefined;
    if (std.mem.eql(u8, a[0], "-c")) {
        if (a.len < 2) std.process.fatal("sh -c: no command", .{});
        source = a[1];
        rest = if (a.len > 3) a[3..] else &.{};
    } else {
        source = shell.dir.readFileAlloc(io, a[0], arena, .limited(16 << 20)) catch |e|
            std.process.fatal("{s}: {t}", .{ a[0], e });
        rest = a[1..];
    }
    shell.positional = try arena.dupe([]const u8, rest);
    var out = Output{};
    var err = Output{};
    const status = shell.runSource(source, .{ .stdin = null, .stdout = &out, .stderr = &err }) catch |e| switch (e) {
        error.Exit => shell.status,
        else => {
            try Io.File.stdout().writeStreamingAll(io, out.bytes.items);
            try Io.File.stderr().writeStreamingAll(io, err.bytes.items);
            std.process.fatal("sh: {t}: {s}", .{ e, shell.why });
        },
    };
    try Io.File.stdout().writeStreamingAll(io, out.bytes.items);
    try Io.File.stderr().writeStreamingAll(io, err.bytes.items);
    std.process.exit(@truncate(status));
}

// ---------------------------------------------------------------------------------------------------------------
// The grammar.

/// A word as written: its text, quotes and all, expanded when it is used.
const Word = []const u8;

const Redirection = struct {
    fd: u8,
    kind: enum { in, out, append, dup, here },
    /// The file, or the descriptor duplicated (`2>&1`), as a word; a here-document's delimiter.
    target: Word,
    /// A here-document's body, and whether it is expanded (its delimiter unquoted).
    body: []const u8 = "",
    expand: bool = true,
};

const Node = union(enum) {
    simple: struct { assignments: []const Word, words: []const Word, redirections: []const Redirection },
    pipeline: struct { commands: []const *Node, negate: bool },
    and_or: struct { left: *Node, op: enum { @"and", @"or" }, right: *Node },
    list: []const *Node,
    subshell: struct { body: *Node, redirections: []const Redirection },
    group: struct { body: *Node, redirections: []const Redirection },
    @"if": struct { conditions: []const *Node, bodies: []const *Node, otherwise: ?*Node, redirections: []const Redirection },
    @"for": struct { name: []const u8, words: ?[]const Word, body: *Node, redirections: []const Redirection },
    case: struct { word: Word, patterns: []const []const Word, bodies: []const ?*Node, redirections: []const Redirection },
    loop: struct { condition: *Node, body: *Node, until: bool, redirections: []const Redirection },
    function: struct { name: []const u8, body: *Node },
};

const ParseError = error{ Syntax, OutOfMemory };

const Parser = struct {
    arena: Allocator,
    src: []const u8,
    pos: usize = 0,
    /// Here-documents read past since the last newline, whose bodies follow the next one.
    pending: std.ArrayList(*Redirection) = .empty,
    why: *[]const u8,

    fn fail(p: *Parser, comptime what: []const u8) ParseError {
        const line = std.mem.count(u8, p.src[0..@min(p.pos, p.src.len)], "\n") + 1;
        p.why.* = std.fmt.allocPrint(p.arena, "line {d}: " ++ what, .{line}) catch "syntax";
        return error.Syntax;
    }

    fn peek(p: *Parser) u8 {
        return if (p.pos < p.src.len) p.src[p.pos] else 0;
    }

    /// Blanks, comments and escaped newlines skipped, newlines kept.
    fn skipBlanks(p: *Parser) void {
        while (p.pos < p.src.len) {
            const c = p.src[p.pos];
            if (c == ' ' or c == '\t' or c == '\r') {
                p.pos += 1;
            } else if (c == '\\' and p.pos + 1 < p.src.len and p.src[p.pos + 1] == '\n') {
                p.pos += 2;
            } else if (c == '#') {
                while (p.pos < p.src.len and p.src[p.pos] != '\n') p.pos += 1;
            } else break;
        }
    }

    /// A newline consumed, and the here-documents waiting on it read.
    fn newline(p: *Parser) ParseError!void {
        p.pos += 1;
        for (p.pending.items) |r| try p.readHereDocument(r);
        p.pending.clearRetainingCapacity();
    }

    /// Blanks and newlines skipped.
    fn skipLinebreaks(p: *Parser) ParseError!void {
        while (true) {
            p.skipBlanks();
            if (p.peek() == '\n') try p.newline() else break;
        }
    }

    fn readHereDocument(p: *Parser, r: *Redirection) ParseError!void {
        var body: std.ArrayList(u8) = .empty;
        while (p.pos < p.src.len) {
            const end = std.mem.indexOfScalarPos(u8, p.src, p.pos, '\n') orelse p.src.len;
            var line = p.src[p.pos..end];
            p.pos = @min(end + 1, p.src.len);
            if (r.target.len > 0 and r.target[0] == '-') {
                while (line.len > 0 and line[0] == '\t') line = line[1..];
            }
            const delimiter = if (r.target.len > 0 and r.target[0] == '-') r.target[1..] else r.target;
            if (std.mem.eql(u8, std.mem.trimEnd(u8, line, "\r"), delimiter)) {
                r.body = body.items;
                return;
            }
            try body.appendSlice(p.arena, line);
            try body.append(p.arena, '\n');
        }
        r.body = body.items;
    }

    fn atOperator(p: *Parser, op: []const u8) bool {
        return std.mem.startsWith(u8, p.src[p.pos..], op);
    }

    /// The next word's extent, quotes and expansions within it kept whole; empty at an operator or the end.
    fn scanWord(p: *Parser) ParseError!Word {
        const start = p.pos;
        while (p.pos < p.src.len) {
            const c = p.src[p.pos];
            switch (c) {
                ' ', '\t', '\r', '\n', ';', '&', '|', '(', ')', '<', '>' => break,
                '\\' => p.pos = @min(p.pos + 2, p.src.len),
                '\'' => {
                    const close = std.mem.indexOfScalarPos(u8, p.src, p.pos + 1, '\'') orelse return p.fail("an unclosed '");
                    p.pos = close + 1;
                },
                '"' => try p.skipDoubleQuoted(),
                '`' => try p.skipBackquoted(),
                '$' => try p.skipDollar(),
                else => p.pos += 1,
            }
        }
        return p.src[start..p.pos];
    }

    fn skipDoubleQuoted(p: *Parser) ParseError!void {
        p.pos += 1;
        while (p.pos < p.src.len) {
            switch (p.src[p.pos]) {
                '"' => {
                    p.pos += 1;
                    return;
                },
                '\\' => p.pos += 2,
                '`' => try p.skipBackquoted(),
                '$' => try p.skipDollar(),
                else => p.pos += 1,
            }
        }
        return p.fail("an unclosed \"");
    }

    fn skipBackquoted(p: *Parser) ParseError!void {
        p.pos += 1;
        while (p.pos < p.src.len) {
            switch (p.src[p.pos]) {
                '`' => {
                    p.pos += 1;
                    return;
                },
                '\\' => p.pos += 2,
                else => p.pos += 1,
            }
        }
        return p.fail("an unclosed `");
    }

    fn skipDollar(p: *Parser) ParseError!void {
        p.pos += 1;
        if (p.peek() == '{') {
            p.pos += 1;
            var depth: usize = 1;
            while (p.pos < p.src.len) {
                switch (p.src[p.pos]) {
                    '}' => {
                        depth -= 1;
                        p.pos += 1;
                        if (depth == 0) return;
                    },
                    '{' => {
                        depth += 1;
                        p.pos += 1;
                    },
                    '\\' => p.pos += 2,
                    '\'' => {
                        const close = std.mem.indexOfScalarPos(u8, p.src, p.pos + 1, '\'') orelse return p.fail("an unclosed '");
                        p.pos = close + 1;
                    },
                    '"' => try p.skipDoubleQuoted(),
                    '`' => try p.skipBackquoted(),
                    '$' => try p.skipDollar(),
                    else => p.pos += 1,
                }
            }
            return p.fail("an unclosed ${{");
        } else if (p.peek() == '(') {
            // `$(...)` and `$((...))`: parentheses balanced, quotes skipped.
            p.pos += 1;
            var depth: usize = 1;
            while (p.pos < p.src.len) {
                switch (p.src[p.pos]) {
                    ')' => {
                        depth -= 1;
                        p.pos += 1;
                        if (depth == 0) return;
                    },
                    '(' => {
                        depth += 1;
                        p.pos += 1;
                    },
                    '\\' => p.pos += 2,
                    '\'' => {
                        const close = std.mem.indexOfScalarPos(u8, p.src, p.pos + 1, '\'') orelse return p.fail("an unclosed '");
                        p.pos = close + 1;
                    },
                    '"' => try p.skipDoubleQuoted(),
                    '`' => try p.skipBackquoted(),
                    else => p.pos += 1,
                }
            }
            return p.fail("an unclosed $(");
        }
    }

    /// The next word if it is this reserved word, unquoted, consumed.
    fn reserved(p: *Parser, word: []const u8) bool {
        p.skipBlanks();
        const save = p.pos;
        const w = p.scanWord() catch {
            p.pos = save;
            return false;
        };
        if (std.mem.eql(u8, w, word)) return true;
        p.pos = save;
        return false;
    }

    fn expectReserved(p: *Parser, word: []const u8) ParseError!void {
        try p.skipLinebreaks();
        if (!p.reserved(word)) {
            p.why.* = std.fmt.allocPrint(p.arena, "line {d}: `{s}' expected", .{
                std.mem.count(u8, p.src[0..p.pos], "\n") + 1, word,
            }) catch "syntax";
            return error.Syntax;
        }
    }

    fn create(p: *Parser, node: Node) ParseError!*Node {
        const n = try p.arena.create(Node);
        n.* = node;
        return n;
    }

    /// A whole program: commands until the end.
    fn program(p: *Parser) ParseError!*Node {
        const list = try p.compoundList(&.{});
        p.skipBlanks();
        if (p.pos < p.src.len) return p.fail("unexpected text");
        return list;
    }

    /// Commands until one of `ends` (a reserved word, or `)`, `;;`) or the end.
    fn compoundList(p: *Parser, ends: []const []const u8) ParseError!*Node {
        var items: std.ArrayList(*Node) = .empty;
        while (true) {
            try p.skipLinebreaks();
            if (p.pos >= p.src.len) break;
            if (p.atEnd(ends)) break;
            const item = try p.andOr();
            try items.append(p.arena, item);
            p.skipBlanks();
            if (p.peek() == ';' and !p.atOperator(";;")) {
                p.pos += 1;
            } else if (p.peek() == '&' and !p.atOperator("&&")) {
                p.pos += 1; // run in the foreground: there are no jobs here
            } else if (p.peek() == '\n') {
                try p.newline();
            } else if (p.pos < p.src.len and !p.atEnd(ends)) {
                return p.fail("unexpected text after a command");
            }
        }
        return p.create(.{ .list = items.items });
    }

    fn atEnd(p: *Parser, ends: []const []const u8) bool {
        p.skipBlanks();
        for (ends) |end| {
            if (std.mem.eql(u8, end, ")") or std.mem.eql(u8, end, ";;")) {
                if (p.atOperator(end)) return true;
                continue;
            }
            const save = p.pos;
            const w = p.scanWord() catch "";
            p.pos = save;
            if (std.mem.eql(u8, w, end)) return true;
        }
        return false;
    }

    fn andOr(p: *Parser) ParseError!*Node {
        var left = try p.pipeline();
        while (true) {
            p.skipBlanks();
            if (p.atOperator("&&")) {
                p.pos += 2;
                try p.skipLinebreaks();
                left = try p.create(.{ .and_or = .{ .left = left, .op = .@"and", .right = try p.pipeline() } });
            } else if (p.atOperator("||")) {
                p.pos += 2;
                try p.skipLinebreaks();
                left = try p.create(.{ .and_or = .{ .left = left, .op = .@"or", .right = try p.pipeline() } });
            } else return left;
        }
    }

    fn pipeline(p: *Parser) ParseError!*Node {
        const negate = p.reserved("!");
        var commands: std.ArrayList(*Node) = .empty;
        try commands.append(p.arena, try p.command());
        while (true) {
            p.skipBlanks();
            if (p.peek() == '|' and !p.atOperator("||")) {
                p.pos += 1;
                try p.skipLinebreaks();
                try commands.append(p.arena, try p.command());
            } else break;
        }
        if (commands.items.len == 1 and !negate) return commands.items[0];
        return p.create(.{ .pipeline = .{ .commands = commands.items, .negate = negate } });
    }

    fn command(p: *Parser) ParseError!*Node {
        p.skipBlanks();
        if (p.peek() == '(') {
            p.pos += 1;
            const body = try p.compoundList(&.{")"});
            p.skipBlanks();
            if (p.peek() != ')') return p.fail("`)' expected");
            p.pos += 1;
            return p.create(.{ .subshell = .{ .body = body, .redirections = try p.redirections() } });
        }
        const save = p.pos;
        const first = try p.scanWord();
        if (std.mem.eql(u8, first, "{")) {
            const body = try p.compoundList(&.{"}"});
            try p.expectReserved("}");
            return p.create(.{ .group = .{ .body = body, .redirections = try p.redirections() } });
        }
        if (std.mem.eql(u8, first, "if")) return p.ifCommand();
        if (std.mem.eql(u8, first, "for")) return p.forCommand();
        if (std.mem.eql(u8, first, "case")) return p.caseCommand();
        if (std.mem.eql(u8, first, "while") or std.mem.eql(u8, first, "until")) {
            const condition = try p.compoundList(&.{"do"});
            try p.expectReserved("do");
            const body = try p.compoundList(&.{"done"});
            try p.expectReserved("done");
            return p.create(.{ .loop = .{
                .condition = condition,
                .body = body,
                .until = first[0] == 'u',
                .redirections = try p.redirections(),
            } });
        }
        // A function: `name ( )` and a compound command.
        if (first.len > 0 and isName(first)) {
            const after = p.pos;
            p.skipBlanks();
            if (p.peek() == '(') {
                p.pos += 1;
                p.skipBlanks();
                if (p.peek() == ')') {
                    p.pos += 1;
                    try p.skipLinebreaks();
                    const body = try p.command();
                    return p.create(.{ .function = .{ .name = first, .body = body } });
                }
            }
            p.pos = after;
        }
        p.pos = save;
        return p.simpleCommand();
    }

    fn simpleCommand(p: *Parser) ParseError!*Node {
        var assignments: std.ArrayList(Word) = .empty;
        var words: std.ArrayList(Word) = .empty;
        var redirs: std.ArrayList(Redirection) = .empty;
        while (true) {
            p.skipBlanks();
            if (try p.redirection()) |r| {
                try redirs.append(p.arena, r);
                continue;
            }
            const word = try p.scanWord();
            if (word.len == 0) break;
            if (words.items.len == 0 and isAssignment(word)) {
                try assignments.append(p.arena, word);
            } else {
                try words.append(p.arena, word);
            }
        }
        if (assignments.items.len == 0 and words.items.len == 0 and redirs.items.len == 0) {
            return p.fail("a command expected");
        }
        const node = try p.create(.{ .simple = .{
            .assignments = assignments.items,
            .words = words.items,
            .redirections = redirs.items,
        } });
        // Here-documents are read after the line: point them at their place in the node.
        for (node.simple.redirections) |*r| {
            if (r.kind == .here) try p.pending.append(p.arena, @constCast(r));
        }
        return node;
    }

    fn redirections(p: *Parser) ParseError![]const Redirection {
        var list: std.ArrayList(Redirection) = .empty;
        while (true) {
            p.skipBlanks();
            const r = try p.redirection() orelse break;
            try list.append(p.arena, r);
        }
        const items = list.items;
        for (items) |*r| {
            if (r.kind == .here) try p.pending.append(p.arena, @constCast(r));
        }
        return items;
    }

    /// A redirection, if one is next: `[n]>`, `>>`, `<`, `>&`, `<<`, `<<-`.
    fn redirection(p: *Parser) ParseError!?Redirection {
        const save = p.pos;
        var fd: ?u8 = null;
        if (p.pos < p.src.len and std.ascii.isDigit(p.src[p.pos]) and p.pos + 1 < p.src.len and
            (p.src[p.pos + 1] == '<' or p.src[p.pos + 1] == '>'))
        {
            fd = p.src[p.pos] - '0';
            p.pos += 1;
        }
        var r: Redirection = undefined;
        if (p.atOperator("<<-")) {
            p.pos += 3;
            r = .{ .fd = fd orelse 0, .kind = .here, .target = "" };
            p.skipBlanks();
            const delimiter = try p.scanWord();
            r.expand = std.mem.indexOfAny(u8, delimiter, "'\"\\") == null;
            r.target = try std.mem.concat(p.arena, u8, &.{ "-", try unquote(p.arena, delimiter) });
            return r;
        } else if (p.atOperator("<<")) {
            p.pos += 2;
            r = .{ .fd = fd orelse 0, .kind = .here, .target = "" };
            p.skipBlanks();
            const delimiter = try p.scanWord();
            r.expand = std.mem.indexOfAny(u8, delimiter, "'\"\\") == null;
            r.target = try unquote(p.arena, delimiter);
            return r;
        } else if (p.atOperator(">>")) {
            p.pos += 2;
            r = .{ .fd = fd orelse 1, .kind = .append, .target = "" };
        } else if (p.atOperator(">&")) {
            p.pos += 2;
            r = .{ .fd = fd orelse 1, .kind = .dup, .target = "" };
        } else if (p.atOperator("<&")) {
            p.pos += 2;
            r = .{ .fd = fd orelse 0, .kind = .dup, .target = "" };
        } else if (p.atOperator(">|") or p.atOperator(">")) {
            p.pos += if (p.atOperator(">|")) 2 else 1;
            r = .{ .fd = fd orelse 1, .kind = .out, .target = "" };
        } else if (p.atOperator("<")) {
            p.pos += 1;
            r = .{ .fd = fd orelse 0, .kind = .in, .target = "" };
        } else {
            p.pos = save;
            return null;
        }
        p.skipBlanks();
        r.target = try p.scanWord();
        if (r.target.len == 0) return p.fail("a redirection without a file");
        return r;
    }

    fn ifCommand(p: *Parser) ParseError!*Node {
        var conditions: std.ArrayList(*Node) = .empty;
        var bodies: std.ArrayList(*Node) = .empty;
        var otherwise: ?*Node = null;
        while (true) {
            try conditions.append(p.arena, try p.compoundList(&.{"then"}));
            try p.expectReserved("then");
            try bodies.append(p.arena, try p.compoundList(&.{ "elif", "else", "fi" }));
            try p.skipLinebreaks();
            if (p.reserved("elif")) continue;
            if (p.reserved("else")) {
                otherwise = try p.compoundList(&.{"fi"});
                try p.expectReserved("fi");
                break;
            }
            try p.expectReserved("fi");
            break;
        }
        return p.create(.{ .@"if" = .{
            .conditions = conditions.items,
            .bodies = bodies.items,
            .otherwise = otherwise,
            .redirections = try p.redirections(),
        } });
    }

    fn forCommand(p: *Parser) ParseError!*Node {
        p.skipBlanks();
        const name = try p.scanWord();
        if (!isName(name)) return p.fail("a name expected after `for'");
        var words: ?[]const Word = null;
        try p.skipLinebreaks();
        if (p.reserved("in")) {
            var list: std.ArrayList(Word) = .empty;
            while (true) {
                p.skipBlanks();
                const w = try p.scanWord();
                if (w.len == 0) break;
                try list.append(p.arena, w);
            }
            words = list.items;
            p.skipBlanks();
            if (p.peek() == ';') p.pos += 1;
        } else {
            p.skipBlanks();
            if (p.peek() == ';') p.pos += 1;
        }
        try p.expectReserved("do");
        const body = try p.compoundList(&.{"done"});
        try p.expectReserved("done");
        return p.create(.{ .@"for" = .{ .name = name, .words = words, .body = body, .redirections = try p.redirections() } });
    }

    fn caseCommand(p: *Parser) ParseError!*Node {
        p.skipBlanks();
        const word = try p.scanWord();
        try p.expectReserved("in");
        var patterns: std.ArrayList([]const Word) = .empty;
        var bodies: std.ArrayList(?*Node) = .empty;
        while (true) {
            try p.skipLinebreaks();
            if (p.reserved("esac")) break;
            p.skipBlanks();
            if (p.peek() == '(') p.pos += 1;
            var alternatives: std.ArrayList(Word) = .empty;
            while (true) {
                p.skipBlanks();
                const pattern = try p.scanWord();
                if (pattern.len == 0) return p.fail("a pattern expected");
                try alternatives.append(p.arena, pattern);
                p.skipBlanks();
                if (p.peek() == '|') {
                    p.pos += 1;
                    continue;
                }
                if (p.peek() == ')') {
                    p.pos += 1;
                    break;
                }
                return p.fail("`)' expected after a pattern");
            }
            const body = try p.compoundList(&.{ ";;", "esac" });
            try patterns.append(p.arena, alternatives.items);
            try bodies.append(p.arena, body);
            p.skipBlanks();
            if (p.atOperator(";;")) {
                p.pos += 2;
                continue;
            }
            try p.expectReserved("esac");
            break;
        }
        return p.create(.{ .case = .{
            .word = word,
            .patterns = patterns.items,
            .bodies = bodies.items,
            .redirections = try p.redirections(),
        } });
    }
};

fn isName(s: []const u8) bool {
    if (s.len == 0 or !(std.ascii.isAlphabetic(s[0]) or s[0] == '_')) return false;
    for (s) |c| if (!(std.ascii.isAlphanumeric(c) or c == '_')) return false;
    return true;
}

fn isAssignment(word: []const u8) bool {
    const eq = std.mem.indexOfScalar(u8, word, '=') orelse return false;
    return isName(word[0..eq]);
}

/// A word's quotes removed, nothing expanded: a here-document's delimiter.
fn unquote(arena: Allocator, word: []const u8) ![]const u8 {
    var out: std.ArrayList(u8) = .empty;
    var i: usize = 0;
    while (i < word.len) : (i += 1) {
        switch (word[i]) {
            '\'', '"' => {},
            '\\' => if (i + 1 < word.len) {
                i += 1;
                try out.append(arena, word[i]);
            },
            else => try out.append(arena, word[i]),
        }
    }
    return out.items;
}

// ---------------------------------------------------------------------------------------------------------------
// Running it.

const Output = struct {
    bytes: std.ArrayList(u8) = .empty,
};

const Context = struct {
    /// What a command reads: the shell's own standard input where null, read when a command first reads it.
    stdin: ?[]const u8,
    stdout: *Output,
    stderr: *Output,
};

const RunError = error{ Exit, Return, Syntax, Failed, OutOfMemory };

/// The shell's standard input, once a command has read it.
var process_stdin: ?[]const u8 = null;

const Shell = struct {
    arena: Allocator,
    io: Io,
    /// Where relative paths are found.
    dir: Io.Dir,
    variables: std.StringHashMapUnmanaged([]const u8) = .empty,
    functions: std.StringHashMapUnmanaged(*Node) = .empty,
    positional: []const []const u8 = &.{},
    status: u32 = 0,
    why: []const u8 = "",

    /// What a command reads.
    fn input(sh: *Shell, ctx: Context) RunError![]const u8 {
        if (ctx.stdin) |bytes| return bytes;
        if (process_stdin == null) {
            var reader: Io.File.Reader = .init(Io.File.stdin(), sh.io, &.{});
            process_stdin = reader.interface.allocRemaining(sh.arena, .unlimited) catch "";
        }
        return process_stdin.?;
    }

    /// `PWD` made the folder relative paths are found in.
    fn setPwd(sh: *Shell) !void {
        var buffer: [std.fs.max_path_bytes]u8 = undefined;
        const n = try sh.dir.realPath(sh.io, &buffer);
        try sh.variables.put(sh.arena, "PWD", try sh.arena.dupe(u8, buffer[0..n]));
    }

    fn fail(sh: *Shell, comptime format: []const u8, args: anytype) RunError {
        sh.why = std.fmt.allocPrint(sh.arena, format, args) catch "?";
        return error.Failed;
    }

    fn runSource(sh: *Shell, source: []const u8, ctx: Context) RunError!u32 {
        var p = Parser{ .arena = sh.arena, .src = source, .why = &sh.why };
        const node = p.program() catch |e| switch (e) {
            error.Syntax => return error.Syntax,
            error.OutOfMemory => return error.OutOfMemory,
        };
        try sh.run(node, ctx);
        return sh.status;
    }

    /// A copy whose changes are its own: a subshell's.
    fn fork(sh: *Shell) RunError!*Shell {
        const copy = try sh.arena.create(Shell);
        copy.* = .{
            .arena = sh.arena,
            .io = sh.io,
            .dir = sh.dir,
            .variables = try sh.variables.clone(sh.arena),
            .functions = try sh.functions.clone(sh.arena),
            .positional = sh.positional,
            .status = sh.status,
        };
        return copy;
    }

    fn run(sh: *Shell, node: *const Node, ctx: Context) RunError!void {
        switch (node.*) {
            .list => |items| for (items) |item| try sh.run(item, ctx),
            .and_or => |a| {
                try sh.run(a.left, ctx);
                if ((sh.status == 0) == (a.op == .@"and")) try sh.run(a.right, ctx);
            },
            .pipeline => |pl| {
                var feed = ctx.stdin;
                for (pl.commands, 0..) |cmd, i| {
                    const last = i + 1 == pl.commands.len;
                    var out = Output{};
                    const sub = try sh.fork();
                    sub.run(cmd, .{ .stdin = feed, .stdout = if (last) ctx.stdout else &out, .stderr = ctx.stderr }) catch |e| switch (e) {
                        error.Exit, error.Return => {},
                        else => return e,
                    };
                    sh.status = sub.status;
                    feed = out.bytes.items;
                }
                if (pl.negate) sh.status = @intFromBool(sh.status == 0);
            },
            .subshell => |s| {
                try sh.redirected(s.redirections, ctx, struct {
                    fn body(shell: *Shell, n: *const Node, c: Context) RunError!void {
                        const sub = try shell.fork();
                        sub.run(n.subshell.body, c) catch |e| switch (e) {
                            error.Exit, error.Return => {},
                            else => return e,
                        };
                        shell.status = sub.status;
                    }
                }.body, node);
            },
            .group => |g| try sh.redirected(g.redirections, ctx, struct {
                fn body(shell: *Shell, n: *const Node, c: Context) RunError!void {
                    try shell.run(n.group.body, c);
                }
            }.body, node),
            .@"if" => |f| try sh.redirected(f.redirections, ctx, struct {
                fn body(shell: *Shell, n: *const Node, c: Context) RunError!void {
                    const i = n.@"if";
                    for (i.conditions, i.bodies) |condition, b| {
                        try shell.run(condition, c);
                        if (shell.status == 0) return shell.run(b, c);
                    }
                    if (i.otherwise) |o| return shell.run(o, c);
                    shell.status = 0;
                }
            }.body, node),
            .@"for" => |f| try sh.redirected(f.redirections, ctx, struct {
                fn body(shell: *Shell, n: *const Node, c: Context) RunError!void {
                    const l = n.@"for";
                    var values: std.ArrayList([]const u8) = .empty;
                    if (l.words) |words| {
                        for (words) |w| try shell.expandFields(w, c, &values);
                    } else try values.appendSlice(shell.arena, shell.positional);
                    shell.status = 0;
                    for (values.items) |v| {
                        try shell.variables.put(shell.arena, l.name, v);
                        try shell.run(l.body, c);
                    }
                }
            }.body, node),
            .case => |cs| try sh.redirected(cs.redirections, ctx, struct {
                fn body(shell: *Shell, n: *const Node, c: Context) RunError!void {
                    const k = n.case;
                    const subject = try shell.expandString(k.word, c);
                    shell.status = 0;
                    for (k.patterns, k.bodies) |alternatives, b| {
                        for (alternatives) |alternative| {
                            const pattern = try shell.expandPattern(alternative, c);
                            if (globMatch(pattern, subject)) {
                                if (b) |bb| try shell.run(bb, c);
                                return;
                            }
                        }
                    }
                }
            }.body, node),
            .loop => |l| try sh.redirected(l.redirections, ctx, struct {
                fn body(shell: *Shell, n: *const Node, c: Context) RunError!void {
                    const lp = n.loop;
                    var guard: usize = 0;
                    while (true) : (guard += 1) {
                        if (guard > 1_000_000) return shell.fail("a loop that does not end", .{});
                        try shell.run(lp.condition, c);
                        if ((shell.status == 0) == lp.until) break;
                        try shell.run(lp.body, c);
                    }
                    shell.status = 0;
                }
            }.body, node),
            .function => |f| {
                try sh.functions.put(sh.arena, f.name, f.body);
                sh.status = 0;
            },
            .simple => try sh.simple(node, ctx),
        }
    }

    /// A command run with its redirections: what it reads and writes arranged, and files written after it ran.
    fn redirected(
        sh: *Shell,
        list: []const Redirection,
        ctx: Context,
        comptime body: fn (*Shell, *const Node, Context) RunError!void,
        node: *const Node,
    ) RunError!void {
        if (list.len == 0) return body(sh, node, ctx);
        var c = ctx;
        var outputs: [10]?*Output = @splat(null);
        var targets: [10]?struct { path: []const u8, append: bool } = @splat(null);
        for (list) |r| {
            switch (r.kind) {
                .in => {
                    const path = try sh.expandString(r.target, ctx);
                    c.stdin = sh.dir.readFileAlloc(sh.io, path, sh.arena, .limited(64 << 20)) catch
                        return sh.fail("{s}: cannot be read", .{path});
                },
                .here => c.stdin = if (r.expand) try sh.expandHere(r.body, ctx) else r.body,
                .out, .append => {
                    const path = try sh.expandString(r.target, ctx);
                    const out = try sh.arena.create(Output);
                    out.* = .{};
                    outputs[r.fd] = out;
                    targets[r.fd] = .{ .path = path, .append = r.kind == .append };
                    if (r.fd == 1) c.stdout = out else if (r.fd == 2) c.stderr = out;
                    // Created (or emptied) when the redirection is made, as a shell does.
                    if (r.kind == .out) sh.writeFile(path, "", false) catch return sh.fail("{s}: cannot be written", .{path});
                },
                .dup => {
                    const to = try sh.expandString(r.target, ctx);
                    if (std.mem.eql(u8, to, "1")) {
                        if (r.fd == 2) c.stderr = c.stdout;
                    } else if (std.mem.eql(u8, to, "2")) {
                        if (r.fd == 1) c.stdout = c.stderr;
                    } else if (!std.mem.eql(u8, to, "-") and !std.mem.eql(u8, to, "0")) {
                        return sh.fail("descriptor {s} is not one this shell has", .{to});
                    }
                },
            }
        }
        const result = body(sh, node, c);
        for (outputs, targets) |out, target| {
            if (out) |o| {
                const t = target.?;
                sh.writeFile(t.path, o.bytes.items, true) catch return sh.fail("{s}: cannot be written", .{t.path});
            }
        }
        return result;
    }

    fn writeFile(sh: *Shell, path: []const u8, bytes: []const u8, append: bool) !void {
        var data = bytes;
        if (append) {
            const before = sh.dir.readFileAlloc(sh.io, path, sh.arena, .limited(256 << 20)) catch "";
            data = try std.mem.concat(sh.arena, u8, &.{ before, bytes });
        }
        if (std.fs.path.dirname(path)) |parent| sh.dir.createDirPath(sh.io, parent) catch {};
        try sh.dir.writeFile(sh.io, .{ .sub_path = path, .data = data });
    }

    fn simple(sh: *Shell, node: *const Node, ctx: Context) RunError!void {
        const s = node.simple;
        var argv: std.ArrayList([]const u8) = .empty;
        for (s.words) |w| try sh.expandFields(w, ctx, &argv);
        // Assignments alone set the shell's variables; before a command they are its own (here: set, as the
        // scripts only use the former).
        for (s.assignments) |a| {
            const eq = std.mem.indexOfScalar(u8, a, '=').?;
            try sh.variables.put(sh.arena, a[0..eq], try sh.expandString(a[eq + 1 ..], ctx));
        }
        if (argv.items.len == 0) {
            const Handler = struct {
                fn body(shell: *Shell, _: *const Node, _: Context) RunError!void {
                    shell.status = 0;
                }
            };
            if (s.redirections.len > 0) return sh.redirected(s.redirections, ctx, Handler.body, node);
            if (s.assignments.len > 0) sh.status = 0;
            return;
        }
        const Call = struct {
            var current: []const []const u8 = &.{};
            fn body(shell: *Shell, _: *const Node, c: Context) RunError!void {
                try shell.call(current, c);
            }
        };
        const saved = Call.current;
        Call.current = argv.items;
        defer Call.current = saved;
        return sh.redirected(s.redirections, ctx, Call.body, node);
    }

    fn call(sh: *Shell, argv: []const []const u8, ctx: Context) RunError!void {
        const name = argv[0];
        const args = argv[1..];
        if (sh.functions.get(name)) |body| {
            const saved = sh.positional;
            sh.positional = args;
            defer sh.positional = saved;
            sh.run(body, ctx) catch |e| switch (e) {
                error.Return => {},
                else => return e,
            };
            return;
        }
        sh.status = 0;
        const eq = std.mem.eql;
        if (eq(u8, name, ":") or eq(u8, name, "true") or eq(u8, name, "export") or eq(u8, name, "readonly")) {
            // `export`: there is no environment to give anything to here.
            if (eq(u8, name, "export")) for (args) |a| {
                if (std.mem.indexOfScalar(u8, a, '=')) |i| try sh.variables.put(sh.arena, a[0..i], a[i + 1 ..]);
            };
        } else if (eq(u8, name, "false")) {
            sh.status = 1;
        } else if (eq(u8, name, "echo")) {
            var out = ctx.stdout;
            var newline = true;
            var rest = args;
            if (rest.len > 0 and eq(u8, rest[0], "-n")) {
                newline = false;
                rest = rest[1..];
            }
            for (rest, 0..) |a, i| {
                if (i > 0) try out.bytes.append(sh.arena, ' ');
                try out.bytes.appendSlice(sh.arena, a);
            }
            if (newline) try out.bytes.append(sh.arena, '\n');
        } else if (eq(u8, name, "printf")) {
            if (args.len == 0) return sh.fail("printf: no format", .{});
            try sh.printf(args[0], args[1..], ctx.stdout);
        } else if (eq(u8, name, "cat")) {
            if (args.len == 0) {
                try ctx.stdout.bytes.appendSlice(sh.arena, try sh.input(ctx));
            } else for (args) |path| {
                if (eq(u8, path, "-")) {
                    try ctx.stdout.bytes.appendSlice(sh.arena, try sh.input(ctx));
                    continue;
                }
                const bytes = sh.dir.readFileAlloc(sh.io, path, sh.arena, .limited(256 << 20)) catch {
                    try ctx.stderr.bytes.print(sh.arena, "cat: {s}: no such file\n", .{path});
                    sh.status = 1;
                    continue;
                };
                try ctx.stdout.bytes.appendSlice(sh.arena, bytes);
            }
        } else if (eq(u8, name, "cmp")) {
            // `cmp [-s] a b`: whether two files hold the same bytes, said by its status alone.
            var files: [2][]const u8 = undefined;
            var n: usize = 0;
            for (args) |a| {
                if (a.len > 1 and a[0] == '-') continue;
                if (n == 2) return sh.fail("cmp: more than two files", .{});
                files[n] = a;
                n += 1;
            }
            if (n != 2) return sh.fail("cmp: two files are compared", .{});
            const x = sh.dir.readFileAlloc(sh.io, files[0], sh.arena, .limited(256 << 20)) catch null;
            const y = sh.dir.readFileAlloc(sh.io, files[1], sh.arena, .limited(256 << 20)) catch null;
            sh.status = if (x == null or y == null) 2 else if (eq(u8, x.?, y.?)) 0 else 1;
        } else if (eq(u8, name, "rm")) {
            for (args) |a| {
                if (a.len > 0 and a[0] == '-') continue;
                sh.dir.deleteFile(sh.io, a) catch {};
            }
        } else if (eq(u8, name, "cd")) {
            if (args.len != 1) return sh.fail("cd: one folder", .{});
            sh.dir = sh.dir.openDir(sh.io, args[0], .{}) catch return sh.fail("cd: {s}", .{args[0]});
            sh.setPwd() catch return sh.fail("cd: the path of {s}", .{args[0]});
        } else if (eq(u8, name, "mkdir")) {
            for (args) |a| {
                if (a.len > 0 and a[0] == '-') continue;
                sh.dir.createDirPath(sh.io, a) catch return sh.fail("mkdir: {s}", .{a});
            }
        } else if (eq(u8, name, "test") or eq(u8, name, "[")) {
            var operands = args;
            if (eq(u8, name, "[")) {
                if (operands.len == 0 or !eq(u8, operands[operands.len - 1], "]")) return sh.fail("[: no ]", .{});
                operands = operands[0 .. operands.len - 1];
            }
            sh.status = if (try sh.testExpression(operands)) 0 else 1;
        } else if (eq(u8, name, "set")) {
            var rest = args;
            if (rest.len > 0 and eq(u8, rest[0], "--")) rest = rest[1..];
            if (rest.len > 0 and rest[0].len > 1 and (rest[0][0] == '-' or rest[0][0] == '+')) {
                // Options (`-e`, `-u`) are taken and ignored: nothing here depends on them.
                return;
            }
            sh.positional = try sh.arena.dupe([]const u8, rest);
        } else if (eq(u8, name, "shift")) {
            const n = if (args.len > 0) std.fmt.parseInt(usize, args[0], 10) catch 1 else 1;
            if (n > sh.positional.len) {
                sh.status = 1;
            } else sh.positional = sh.positional[n..];
        } else if (eq(u8, name, "unset")) {
            for (args) |a| {
                if (eq(u8, a, "-v") or eq(u8, a, "-f")) continue;
                _ = sh.variables.remove(a);
                _ = sh.functions.remove(a);
            }
        } else if (eq(u8, name, "exit")) {
            sh.status = if (args.len > 0) std.fmt.parseInt(u32, args[0], 10) catch 2 else sh.status;
            return error.Exit;
        } else if (eq(u8, name, "return")) {
            sh.status = if (args.len > 0) std.fmt.parseInt(u32, args[0], 10) catch 2 else sh.status;
            return error.Return;
        } else if (eq(u8, name, ".") or eq(u8, name, "source")) {
            if (args.len == 0) return sh.fail(".: no file", .{});
            const source = sh.dir.readFileAlloc(sh.io, args[0], sh.arena, .limited(16 << 20)) catch
                return sh.fail("{s}: cannot be read", .{args[0]});
            const saved = sh.positional;
            if (args.len > 1) sh.positional = args[1..];
            defer if (args.len > 1) {
                sh.positional = saved;
            };
            _ = sh.runSource(source, ctx) catch |e| switch (e) {
                error.Return => {},
                error.Syntax => return sh.fail("{s}: {s}", .{ args[0], sh.why }),
                else => return e,
            };
        } else if (eq(u8, name, "eval")) {
            const source = try std.mem.join(sh.arena, " ", args);
            _ = sh.runSource(source, ctx) catch |e| switch (e) {
                error.Syntax => return sh.fail("eval: {s}", .{sh.why}),
                else => return e,
            };
        } else if (eq(u8, name, "sed")) {
            const reads_stdin = sed.readsInput(args);
            sh.status = sed.run(sh.arena, sh.io, sh.dir, args, if (reads_stdin) try sh.input(ctx) else "", &ctx.stdout.bytes) catch |e|
                return sh.fail("sed: {t}", .{e});
        } else {
            return sh.fail("{s}: not a command this shell has", .{name});
        }
    }

    fn printf(sh: *Shell, format: []const u8, args: []const []const u8, out: *Output) RunError!void {
        var next: usize = 0;
        var i: usize = 0;
        while (i < format.len) : (i += 1) {
            const c = format[i];
            if (c == '\\' and i + 1 < format.len) {
                i += 1;
                try out.bytes.append(sh.arena, switch (format[i]) {
                    'n' => '\n',
                    't' => '\t',
                    '\\' => '\\',
                    else => |o| o,
                });
            } else if (c == '%' and i + 1 < format.len) {
                i += 1;
                switch (format[i]) {
                    's' => {
                        if (next < args.len) try out.bytes.appendSlice(sh.arena, args[next]);
                        next += 1;
                    },
                    '%' => try out.bytes.append(sh.arena, '%'),
                    else => return sh.fail("printf: %{c} is not a conversion this shell has", .{format[i]}),
                }
            } else try out.bytes.append(sh.arena, c);
        }
    }

    fn testExpression(sh: *Shell, args: []const []const u8) RunError!bool {
        const eq = std.mem.eql;
        if (args.len == 0) return false;
        if (eq(u8, args[0], "!")) return !try sh.testExpression(args[1..]);
        // `a -a b`, `a -o b`.
        for (args, 0..) |a, i| {
            if (i > 0 and i + 1 < args.len and eq(u8, a, "-o")) return try sh.testExpression(args[0..i]) or try sh.testExpression(args[i + 1 ..]);
        }
        for (args, 0..) |a, i| {
            if (i > 0 and i + 1 < args.len and eq(u8, a, "-a")) return try sh.testExpression(args[0..i]) and try sh.testExpression(args[i + 1 ..]);
        }
        switch (args.len) {
            1 => return args[0].len != 0,
            2 => {
                const op = args[0];
                const v = args[1];
                if (eq(u8, op, "-n")) return v.len != 0;
                if (eq(u8, op, "-z")) return v.len == 0;
                if (eq(u8, op, "-d") or eq(u8, op, "-f") or eq(u8, op, "-e") or eq(u8, op, "-r") or eq(u8, op, "-s")) {
                    const st = sh.dir.statFile(sh.io, v, .{}) catch return false;
                    if (eq(u8, op, "-d")) return st.kind == .directory;
                    if (eq(u8, op, "-f")) return st.kind == .file;
                    if (eq(u8, op, "-s")) return st.size > 0;
                    return true;
                }
                return sh.fail("test: {s} is not an operator this shell has", .{op});
            },
            3 => {
                const a = args[0];
                const op = args[1];
                const b = args[2];
                if (eq(u8, op, "=") or eq(u8, op, "==")) return eq(u8, a, b);
                if (eq(u8, op, "!=")) return !eq(u8, a, b);
                const x = std.fmt.parseInt(i64, a, 10) catch return sh.fail("test: {s} is no number", .{a});
                const y = std.fmt.parseInt(i64, b, 10) catch return sh.fail("test: {s} is no number", .{b});
                if (eq(u8, op, "-eq")) return x == y;
                if (eq(u8, op, "-ne")) return x != y;
                if (eq(u8, op, "-lt")) return x < y;
                if (eq(u8, op, "-le")) return x <= y;
                if (eq(u8, op, "-gt")) return x > y;
                if (eq(u8, op, "-ge")) return x >= y;
                return sh.fail("test: {s} is not an operator this shell has", .{op});
            },
            else => return sh.fail("test: {d} operands is more than this shell reads", .{args.len}),
        }
    }

    // -----------------------------------------------------------------------------------------------------------
    // Expansions.

    /// A piece of an expanded word: whether field splitting may part it.
    const Piece = struct { text: []const u8, split: bool, quoted: bool };

    /// A word expanded to fields: parameters, command substitution, splitting, quotes removed.
    fn expandFields(sh: *Shell, word: Word, ctx: Context, out: *std.ArrayList([]const u8)) RunError!void {
        var pieces: std.ArrayList(Piece) = .empty;
        var at_fields: ?[]const []const u8 = null;
        try sh.expandPieces(word, ctx, &pieces, false, &at_fields);
        var field: std.ArrayList(u8) = .empty;
        var has = false;
        for (pieces.items) |piece| {
            if (piece.text.ptr == at_marker.ptr and piece.quoted) {
                // `"$@"`: each parameter a field of its own.
                const params = at_fields.?;
                for (params, 0..) |param, i| {
                    try field.appendSlice(sh.arena, param);
                    has = true;
                    if (i + 1 < params.len) {
                        try out.append(sh.arena, field.items);
                        field = .empty;
                    }
                }
                continue;
            }
            if (!piece.split) {
                try field.appendSlice(sh.arena, piece.text);
                if (piece.quoted) has = true;
                if (piece.text.len > 0) has = true;
                continue;
            }
            var i: usize = 0;
            while (i < piece.text.len) {
                const c = piece.text[i];
                if (c == ' ' or c == '\t' or c == '\n') {
                    if (has) {
                        try out.append(sh.arena, field.items);
                        field = .empty;
                        has = false;
                    }
                    i += 1;
                    continue;
                }
                try field.append(sh.arena, c);
                has = true;
                i += 1;
            }
        }
        if (has) try out.append(sh.arena, field.items);
    }

    /// A word expanded to one string, unsplit: an assignment's value, a redirection's file, a case's subject.
    fn expandString(sh: *Shell, word: Word, ctx: Context) RunError![]const u8 {
        var pieces: std.ArrayList(Piece) = .empty;
        var at_fields: ?[]const []const u8 = null;
        try sh.expandPieces(word, ctx, &pieces, false, &at_fields);
        var out: std.ArrayList(u8) = .empty;
        for (pieces.items) |piece| {
            if (piece.text.ptr == at_marker.ptr) {
                try out.appendSlice(sh.arena, try std.mem.join(sh.arena, " ", at_fields.?));
            } else try out.appendSlice(sh.arena, piece.text);
        }
        return out.items;
    }

    /// A case pattern: expanded, with what was quoted made literal (its `*`, `?` and `[` escaped).
    fn expandPattern(sh: *Shell, word: Word, ctx: Context) RunError![]const u8 {
        var pieces: std.ArrayList(Piece) = .empty;
        var at_fields: ?[]const []const u8 = null;
        try sh.expandPieces(word, ctx, &pieces, true, &at_fields);
        var out: std.ArrayList(u8) = .empty;
        for (pieces.items) |piece| {
            const text = if (piece.text.ptr == at_marker.ptr) try std.mem.join(sh.arena, " ", at_fields.?) else piece.text;
            if (piece.quoted) {
                for (text) |c| {
                    if (c == '*' or c == '?' or c == '[' or c == '\\') try out.append(sh.arena, '\\');
                    try out.append(sh.arena, c);
                }
            } else try out.appendSlice(sh.arena, text);
        }
        return out.items;
    }

    const at_marker: []const u8 = "\x00@";

    fn expandPieces(
        sh: *Shell,
        word: Word,
        ctx: Context,
        pieces: *std.ArrayList(Piece),
        pattern: bool,
        at_fields: *?[]const []const u8,
    ) RunError!void {
        var i: usize = 0;
        var literal: std.ArrayList(u8) = .empty;
        const flush = struct {
            fn f(s: *Shell, lit: *std.ArrayList(u8), list: *std.ArrayList(Piece)) RunError!void {
                if (lit.items.len > 0) {
                    try list.append(s.arena, .{ .text = lit.items, .split = false, .quoted = false });
                    lit.* = .empty;
                }
            }
        }.f;
        while (i < word.len) {
            const c = word[i];
            switch (c) {
                '\'' => {
                    try flush(sh, &literal, pieces);
                    const close = std.mem.indexOfScalarPos(u8, word, i + 1, '\'') orelse word.len;
                    try pieces.append(sh.arena, .{ .text = word[i + 1 .. close], .split = false, .quoted = true });
                    i = close + 1;
                },
                '"' => {
                    try flush(sh, &literal, pieces);
                    i += 1;
                    var inner: std.ArrayList(u8) = .empty;
                    var any = false;
                    while (i < word.len and word[i] != '"') {
                        const d = word[i];
                        if (d == '\\' and i + 1 < word.len and std.mem.indexOfScalar(u8, "$`\"\\\n", word[i + 1]) != null) {
                            if (word[i + 1] != '\n') try inner.append(sh.arena, word[i + 1]);
                            i += 2;
                        } else if (d == '$' or d == '`') {
                            const end = try sh.expansionEnd(word, i);
                            if (d == '$' and end == i + 1) {
                                // A `$` that starts no expansion is itself.
                                try inner.append(sh.arena, '$');
                                i += 1;
                                continue;
                            }
                            if (d == '$' and std.mem.eql(u8, word[i..end], "$@")) {
                                if (inner.items.len > 0) {
                                    try pieces.append(sh.arena, .{ .text = inner.items, .split = false, .quoted = true });
                                    inner = .empty;
                                }
                                at_fields.* = sh.positional;
                                try pieces.append(sh.arena, .{ .text = at_marker, .split = false, .quoted = true });
                                any = true;
                            } else {
                                try inner.appendSlice(sh.arena, try sh.expandOne(word[i..end], ctx, true));
                            }
                            i = end;
                        } else {
                            try inner.append(sh.arena, d);
                            i += 1;
                        }
                    }
                    i += 1;
                    if (inner.items.len > 0 or !any) {
                        try pieces.append(sh.arena, .{ .text = inner.items, .split = false, .quoted = true });
                    }
                },
                '\\' => {
                    if (i + 1 < word.len) {
                        if (word[i + 1] == '\n') {
                            i += 2;
                            continue;
                        }
                        if (pattern) {
                            try literal.append(sh.arena, '\\');
                        }
                        try literal.append(sh.arena, word[i + 1]);
                        i += 2;
                    } else i += 1;
                },
                '$', '`' => {
                    const end = try sh.expansionEnd(word, i);
                    if (end == i + 1 and c == '$') {
                        try literal.append(sh.arena, '$');
                        i += 1;
                        continue;
                    }
                    try flush(sh, &literal, pieces);
                    const text = try sh.expandOne(word[i..end], ctx, false);
                    try pieces.append(sh.arena, .{ .text = text, .split = !pattern, .quoted = false });
                    i = end;
                },
                else => {
                    try literal.append(sh.arena, c);
                    i += 1;
                },
            }
        }
        try flush(sh, &literal, pieces);
    }

    /// Where the expansion starting at `i` (`$...` or a backquote) ends.
    fn expansionEnd(sh: *Shell, word: []const u8, i: usize) RunError!usize {
        var p = Parser{ .arena = sh.arena, .src = word, .pos = i, .why = &sh.why };
        if (word[i] == '`') {
            p.skipBackquoted() catch return error.Syntax;
            return p.pos;
        }
        if (i + 1 >= word.len) return i + 1;
        const n = word[i + 1];
        if (n == '{' or n == '(') {
            p.skipDollar() catch return error.Syntax;
            return p.pos;
        }
        if (std.ascii.isAlphabetic(n) or n == '_') {
            var j = i + 1;
            while (j < word.len and (std.ascii.isAlphanumeric(word[j]) or word[j] == '_')) j += 1;
            return j;
        }
        if (std.ascii.isDigit(n) or std.mem.indexOfScalar(u8, "#?$@*!-", n) != null) return i + 2;
        return i + 1;
    }

    /// One expansion's value: `$name`, `${...}`, `$(...)`, `` `...` ``.
    fn expandOne(sh: *Shell, text: []const u8, ctx: Context, in_quotes: bool) RunError![]const u8 {
        if (text[0] == '`') {
            // Inside backquotes, a backslash quotes `$`, `` ` `` and `\` only.
            var inner: std.ArrayList(u8) = .empty;
            var i: usize = 1;
            while (i + 1 < text.len) : (i += 1) {
                if (text[i] == '\\' and i + 2 < text.len and (text[i + 1] == '$' or text[i + 1] == '`' or text[i + 1] == '\\' or (in_quotes and text[i + 1] == '"'))) {
                    i += 1;
                }
                try inner.append(sh.arena, text[i]);
            }
            return sh.substitute(inner.items, ctx);
        }
        if (text.len > 2 and text[1] == '(') {
            if (text.len > 4 and text[2] == '(') return sh.fail("arithmetic expansion is not one this shell has", .{});
            return sh.substitute(text[2 .. text.len - 1], ctx);
        }
        if (text.len > 1 and text[1] == '{') return sh.braced(text[2 .. text.len - 1], ctx);
        return sh.parameter(text[1..]) orelse "";
    }

    /// What a command writes, its trailing newlines removed.
    fn substitute(sh: *Shell, source: []const u8, ctx: Context) RunError![]const u8 {
        const sub = try sh.fork();
        var out = Output{};
        _ = sub.runSource(source, .{ .stdin = ctx.stdin, .stdout = &out, .stderr = ctx.stderr }) catch |e| switch (e) {
            error.Exit, error.Return => 0,
            else => {
                sh.why = sub.why;
                return e;
            },
        };
        sh.status = sub.status;
        return std.mem.trimEnd(u8, out.bytes.items, "\n");
    }

    fn parameter(sh: *Shell, name: []const u8) ?[]const u8 {
        if (name.len == 0) return null;
        if (std.ascii.isDigit(name[0])) {
            const n = std.fmt.parseInt(usize, name, 10) catch return null;
            if (n == 0) return "sh";
            return if (n <= sh.positional.len) sh.positional[n - 1] else null;
        }
        if (std.mem.eql(u8, name, "#")) return std.fmt.allocPrint(sh.arena, "{d}", .{sh.positional.len}) catch null;
        if (std.mem.eql(u8, name, "?")) return std.fmt.allocPrint(sh.arena, "{d}", .{sh.status}) catch null;
        // The process: any number does, as it only names a file within one run.
        if (std.mem.eql(u8, name, "$")) return "4242";
        if (std.mem.eql(u8, name, "@") or std.mem.eql(u8, name, "*")) return std.mem.join(sh.arena, " ", sh.positional) catch null;
        if (std.mem.eql(u8, name, "-") or std.mem.eql(u8, name, "!")) return "";
        return sh.variables.get(name);
    }

    /// `${...}`.
    fn braced(sh: *Shell, inner: []const u8, ctx: Context) RunError![]const u8 {
        if (inner.len > 1 and inner[0] == '#') {
            const v = sh.parameter(inner[1..]) orelse "";
            return std.fmt.allocPrint(sh.arena, "{d}", .{v.len});
        }
        var end: usize = 0;
        if (inner.len > 0 and (std.ascii.isDigit(inner[0]) or std.mem.indexOfScalar(u8, "#?$@*!-", inner[0]) != null)) {
            end = 1;
            while (end < inner.len and std.ascii.isDigit(inner[0]) and std.ascii.isDigit(inner[end])) end += 1;
        } else {
            while (end < inner.len and (std.ascii.isAlphanumeric(inner[end]) or inner[end] == '_')) end += 1;
        }
        const name = inner[0..end];
        const value = sh.parameter(name);
        if (end == inner.len) return value orelse "";
        var op = inner[end .. end + 1];
        var rest = inner[end + 1 ..];
        var colon = false;
        if (op[0] == ':' and rest.len > 0) {
            colon = true;
            op = rest[0..1];
            rest = rest[1..];
        }
        const unset = value == null or (colon and value.?.len == 0);
        switch (op[0]) {
            '-' => return if (unset) try sh.expandString(rest, ctx) else value.?,
            '+' => return if (unset) "" else try sh.expandString(rest, ctx),
            '=' => {
                if (unset) {
                    const v = try sh.expandString(rest, ctx);
                    try sh.variables.put(sh.arena, name, v);
                    return v;
                }
                return value.?;
            },
            '?' => {
                if (unset) return sh.fail("{s}: {s}", .{ name, try sh.expandString(rest, ctx) });
                return value.?;
            },
            '%', '#' => {
                const v = value orelse "";
                const longest = rest.len > 0 and rest[0] == op[0];
                const pattern = try sh.expandPattern(if (longest) rest[1..] else rest, ctx);
                if (op[0] == '%') {
                    var cut: ?usize = null;
                    var i: usize = v.len + 1;
                    while (i > 0) {
                        i -= 1;
                        if (globMatch(pattern, v[i..])) {
                            cut = i;
                            if (!longest) break;
                        }
                    }
                    return if (cut) |c| v[0..c] else v;
                }
                var cut: ?usize = null;
                var i: usize = 0;
                while (i <= v.len) : (i += 1) {
                    if (globMatch(pattern, v[0..i])) {
                        cut = i;
                        if (!longest) break;
                    }
                }
                return if (cut) |c| v[c..] else v;
            },
            else => return sh.fail("${{{s}}} is not an expansion this shell has", .{inner}),
        }
    }

    /// A here-document's body expanded: parameters and commands, and a backslash quoting `$`, `` ` ``, `\` and a
    /// newline.
    fn expandHere(sh: *Shell, body: []const u8, ctx: Context) RunError![]const u8 {
        var out: std.ArrayList(u8) = .empty;
        var i: usize = 0;
        while (i < body.len) {
            const c = body[i];
            if (c == '\\' and i + 1 < body.len and std.mem.indexOfScalar(u8, "$`\\\n", body[i + 1]) != null) {
                if (body[i + 1] != '\n') try out.append(sh.arena, body[i + 1]);
                i += 2;
            } else if (c == '$' or c == '`') {
                const end = try sh.expansionEnd(body, i);
                if (end == i + 1 and c == '$') {
                    try out.append(sh.arena, '$');
                    i += 1;
                    continue;
                }
                try out.appendSlice(sh.arena, try sh.expandOne(body[i..end], ctx, true));
                i = end;
            } else {
                try out.append(sh.arena, c);
                i += 1;
            }
        }
        return out.items;
    }
};

/// Whether `text` matches the shell pattern `pattern`: `*`, `?`, `[...]`, and `\` quoting the next character.
fn globMatch(pattern: []const u8, text: []const u8) bool {
    var p: usize = 0;
    var t: usize = 0;
    var star_p: ?usize = null;
    var star_t: usize = 0;
    while (t < text.len) {
        if (p < pattern.len) {
            switch (pattern[p]) {
                '*' => {
                    star_p = p;
                    star_t = t;
                    p += 1;
                    continue;
                },
                '?' => {
                    p += 1;
                    t += 1;
                    continue;
                },
                '[' => if (bracket(pattern[p..], text[t])) |len| {
                    p += len;
                    t += 1;
                    continue;
                },
                '\\' => if (p + 1 < pattern.len and pattern[p + 1] == text[t]) {
                    p += 2;
                    t += 1;
                    continue;
                },
                else => |c| if (c == text[t]) {
                    p += 1;
                    t += 1;
                    continue;
                },
            }
        }
        if (star_p) |sp| {
            p = sp + 1;
            star_t += 1;
            t = star_t;
            continue;
        }
        return false;
    }
    while (p < pattern.len and pattern[p] == '*') p += 1;
    return p == pattern.len;
}

/// A bracket expression at the pattern's start matched against `c`: its length if it matches.
fn bracket(pattern: []const u8, c: u8) ?usize {
    var i: usize = 1;
    var negate = false;
    if (i < pattern.len and (pattern[i] == '!' or pattern[i] == '^')) {
        negate = true;
        i += 1;
    }
    var matched = false;
    var first = true;
    while (i < pattern.len and (first or pattern[i] != ']')) : (first = false) {
        if (i + 2 < pattern.len and pattern[i + 1] == '-' and pattern[i + 2] != ']') {
            if (c >= pattern[i] and c <= pattern[i + 2]) matched = true;
            i += 3;
        } else {
            if (pattern[i] == c) matched = true;
            i += 1;
        }
    }
    if (i >= pattern.len) return null;
    return if (matched != negate) i + 1 else null;
}
