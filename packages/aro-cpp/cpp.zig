//! A C preprocessor taking what GCC's `cc1 -E` takes, made of aro - the C front end zig carries, in zig - for what
//! `pe2.psx` compiles: a modern GCC is a large program to fetch for its preprocessor alone.
//!
//! `cpp --version` says what it is; `cpp -E [-quiet] [-P] [-undef] [-nostdinc] [-Wall] [-Werror] [-I<dir>]... [-iquote <dir>]... [-isystem <dir>]...
//! [-D<name>[=<value>]]... [-U<name>]...
//! [-MMD <file>] [-MF <file>] [-MT <target>] [-MP] -o <output> <input>`: the input preprocessed into the output, with
//! line markers unless `-P`; with `-MMD`/`-MF`, the files it read written as a makefile rule for `-MT` (the output by
//! default), and with `-MP` an empty rule for each, as GCC writes them. No system header is searched (`-nostdinc` is
//! always so) and no target's macros are defined (`-undef` likewise): what is preprocessed is for the PlayStation, and
//! says what it needs.

const std = @import("std");
const aro = @import("aro");
const Compilation = aro.Compilation;
const Diagnostics = aro.Diagnostics;
const Driver = aro.Driver;
const Preprocessor = aro.Preprocessor;
const Toolchain = aro.Toolchain;
/// aro's dependency file, which it does not name outside itself.
const DepFile = @typeInfo(@typeInfo(@FieldType(Preprocessor, "dep_file")).optional.child).pointer.child;

pub fn main(init: std.process.Init) !u8 {
    const gpa = init.gpa;
    const arena = init.arena.allocator();
    const io = init.io;
    const args = try init.minimal.args.toSlice(arena);

    // What GCC's cc1 is asked, read here; what aro's driver takes, given to it.
    var driver_args: std.ArrayList([]const u8) = .empty;
    try driver_args.appendSlice(arena, &.{ "cpp", "-E", "-nostdinc", "-undef" });
    var output: ?[]const u8 = null;
    var input: ?[]const u8 = null;
    var dep_path: ?[]const u8 = null;
    var dep_target: ?[]const u8 = null;
    var phony = false;
    var i: usize = 1;
    while (i < args.len) : (i += 1) {
        const a = args[i];
        const eq = std.mem.eql;
        if (eq(u8, a, "--version")) {
            // What a build checks before it relies on the program.
            try std.Io.File.stdout().writeStreamingAll(io, "cpp: the C preprocessor of aro, as zig " ++ @import("builtin").zig_version_string ++ " carries it\n");
            return 0;
        }
        if (eq(u8, a, "-E") or eq(u8, a, "-quiet") or eq(u8, a, "-Wall") or eq(u8, a, "-undef") or eq(u8, a, "-nostdinc")) {
            continue;
        } else if (eq(u8, a, "-P") or eq(u8, a, "-Werror")) {
            try driver_args.append(arena, a);
        } else if (eq(u8, a, "-MMD") or eq(u8, a, "-MF") or eq(u8, a, "-MT") or eq(u8, a, "-o")) {
            i += 1;
            if (i >= args.len) std.process.fatal("{s} names a file", .{a});
            if (eq(u8, a, "-o")) output = args[i] else if (eq(u8, a, "-MT")) dep_target = args[i] else dep_path = args[i];
        } else if (eq(u8, a, "-MP")) {
            phony = true;
        } else if (eq(u8, a, "-iquote") or eq(u8, a, "-isystem")) {
            i += 1;
            if (i >= args.len) std.process.fatal("{s} names a folder", .{a});
            try driver_args.appendSlice(arena, &.{ a, args[i] });
        } else if (std.mem.startsWith(u8, a, "-I") or std.mem.startsWith(u8, a, "-D") or std.mem.startsWith(u8, a, "-U")) {
            if (a.len == 2) {
                i += 1;
                if (i >= args.len) std.process.fatal("{s} names something", .{a});
                try driver_args.append(arena, try std.mem.concat(arena, u8, &.{ a, args[i] }));
            } else try driver_args.append(arena, a);
        } else if (a.len > 0 and a[0] == '-') {
            std.process.fatal("{s} is not an option this preprocessor takes", .{a});
        } else if (input == null) {
            input = a;
        } else std.process.fatal("{s}: one input is preprocessed at a time", .{a});
    }
    const in = input orelse std.process.fatal("no input", .{});
    const out = output orelse std.process.fatal("no output (-o)", .{});
    try driver_args.append(arena, in);

    var stderr_buf: [1024]u8 = undefined;
    var stderr = std.Io.File.stderr().writer(io, &stderr_buf);
    var diagnostics: Diagnostics = .{ .output = .{ .to_writer = .{ .mode = .no_color, .writer = &stderr.interface } } };
    var comp = try Compilation.init(.{
        .gpa = gpa,
        .arena = arena,
        .io = io,
        .diagnostics = &diagnostics,
        .environ_map = init.environ_map,
    });
    defer comp.deinit();
    var driver: Driver = .{ .comp = &comp, .aro_name = "cpp", .diagnostics = &diagnostics };
    defer driver.deinit();
    var toolchain: Toolchain = .{ .driver = &driver };
    defer toolchain.deinit();

    var macro_buf: std.ArrayList(u8) = .empty;
    var stdout_buf: [256]u8 = undefined;
    var stdout = std.Io.File.stdout().writer(io, &stdout_buf);
    if (try driver.parseArgs(&stdout.interface, &macro_buf, driver_args.items)) return 0;
    const user_macros = try comp.addSourceFromOwnedBuffer("<command line>", try macro_buf.toOwnedSliceSentinel(gpa, 0), .user);
    try toolchain.discover();
    try toolchain.defineSystemIncludes();
    try comp.initSearchPath(driver.includes.items, false);
    const builtin_macros = try comp.generateBuiltinMacros(driver.system_defines);
    const source = driver.inputs.items[0];

    var pp = try Preprocessor.init(&comp, .{ .base_file = source.id });
    defer pp.deinit();
    var deps: DepFile = .{ .target = dep_target orelse out, .format = .make };
    defer deps.deinit(gpa);
    try deps.addDependency(gpa, source.path);
    if (dep_path != null) pp.dep_file = &deps;
    pp.preserve_whitespace = true;
    if (driver.line_commands) pp.linemarkers = .numeric_directives;
    try pp.preprocessSources(.{
        .main = source,
        .builtin = builtin_macros,
        .command_line = user_macros,
        .imacros = driver.imacros.items,
        .implicit_includes = driver.implicit_includes.items,
    });
    driver.printDiagnosticsStats();
    if (diagnostics.errors != 0) return 1;

    var buf: [4096]u8 = undefined;
    {
        const file = try std.Io.Dir.cwd().createFile(io, out, .{});
        defer file.close(io);
        var writer = file.writer(io, &buf);
        try pp.prettyPrintTokens(&writer.interface, .result_only);
        try writer.interface.flush();
    }
    if (dep_path) |path| {
        const file = try std.Io.Dir.cwd().createFile(io, path, .{});
        defer file.close(io);
        var writer = file.writer(io, &buf);
        try deps.write(&writer.interface);
        // `-MP`: an empty rule for each file but the input, so that one removed does not stop make.
        if (phony) for (deps.deps.keys()[1..]) |dep| {
            try writer.interface.print("\n{s}:\n", .{dep});
        };
        try writer.interface.flush();
    }
    return 0;
}
