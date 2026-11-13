//! GNU binutils 2.47 for `mipsel-linux-gnu`: `as`, `ld`, `nm` and `objcopy`, built with zig for any host it targets.
//!
//! What binutils' `configure` and `Makefile`s do, done here. What configure decides of a host is in `hosts/` - its
//! answers for Linux, macOS and Windows, which the parts' `config.h` are; what it decides of the target is written
//! below (BFD's MIPS targets, gas's MIPS assembler, ld's MIPS emulations). The files the Makefiles make are made as
//! they make them: BFD's headers from their templates, and with `sh` - the tools package's shell, run on the machine
//! building - `targmatch.h` with BFD's own sed script, and ld's emulations with its own `genscripts.sh`. Where the
//! source is wrong for a host, `patches/` says how and mends it.

const std = @import("std");

/// The release, as its version string names it: binutils 2.47 calls itself a development build, dated.
const version_string = "2.47.20260726";
const version_number = "247000000";
const prefix = "/usr/local";
const target_canonical = "mipsel-unknown-linux-gnu";
const target_alias = "mipsel-linux-gnu";

/// ld's emulations: its own for the target first, then those its configuration adds.
const emulations = [_][]const u8{ "elf32ltsmip", "elf32btsmip", "elf32ltsmipn32", "elf64ltsmip", "elf32btsmipn32", "elf64btsmip" };

/// BFD's target vectors, its default first.
const vectors = [_][]const u8{
    "mips_elf32_trad_le_vec", "mips_elf32_trad_be_vec",  "mips_ecoff_le_vec",      "mips_ecoff_be_vec",
    "mips_elf32_ntrad_le_vec", "mips_elf64_trad_le_vec", "mips_elf32_ntrad_be_vec", "mips_elf64_trad_be_vec",
    "elf64_le_vec",           "elf64_be_vec",            "elf32_le_vec",           "elf32_be_vec",
};

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    const upstream = b.dependency("binutils", .{});
    const tools = b.dependency("tools", .{});
    const sh = tools.artifact("sh");
    const src = upstream.path("");

    // The shared headers, with this port's patches (`patches/`): used in place of upstream's by every part.
    const patching = b.addRunArtifact(tools.artifact("patch"));
    patching.addDirectoryArg(upstream.path("include"));
    const include = patching.addOutputDirectoryArg("include");
    patching.addPrefixedFileArg("file:same-inode.h=", b.path("patches/windows-same-inode.patch"));

    const host: []const u8 = switch (target.result.os.tag) {
        .linux => "linux",
        .macos => "macos",
        .windows => "windows",
        else => std.debug.panic("binutils is built here for Linux, macOS and Windows, not {t}", .{target.result.os.tag}),
    };
    const windows = target.result.os.tag == .windows;

    // Each part's `config.h`, in a folder of its own.
    const config = struct {
        fn dir(bb: *std.Build, h: []const u8, part: []const u8) std.Build.LazyPath {
            const files = bb.addWriteFiles();
            _ = files.addCopyFile(bb.path(bb.fmt("hosts/{s}/{s}.h", .{ h, part })), "config.h");
            return files.getDirectory();
        }
    };

    // BFD's generated headers, which every part reads.
    const bfd_h = b.addConfigHeader(.{
        .style = .{ .autoconf_at = upstream.path("bfd/bfd-in2.h") },
        .include_path = "bfd.h",
    }, .{
        .supports_plugins = 0,
        .wordsize = 64,
        .bfd_default_target_size = 32,
        .bfd_file_ptr = "int64_t",
        .bfd_ufile_ptr = "uint64_t",
    });
    const bfdver_h = b.addConfigHeader(.{
        .style = .{ .autoconf_at = upstream.path("bfd/version.h") },
        .include_path = "bfdver.h",
    }, .{
        .bfd_version = version_number,
        .bfd_version_package = "\"(GNU Binutils) \"",
        .bfd_version_string = "\"" ++ version_string ++ "\"",
        .report_bugs_to = "\"<https://sourceware.org/bugzilla/>\"",
    });
    const bfd_generated = b.addWriteFiles();
    _ = bfd_generated.addCopyFile(bfd_h.getOutputFile(), "bfd.h");
    _ = bfd_generated.addCopyFile(bfdver_h.getOutputFile(), "bfdver.h");
    for ([_][]const u8{ "32", "64" }) |size| {
        const run = b.addRunArtifact(sh);
        run.addArgs(&.{ "sed", "-e", b.fmt("s/NN/{s}/g", .{size}) });
        run.addFileArg(upstream.path("bfd/elfxx-target.h"));
        _ = bfd_generated.addCopyFile(run.captureStdOut(.{}), b.fmt("elf{s}-target.h", .{size}));
    }
    {
        const run = b.addRunArtifact(sh);
        run.addArgs(&.{ "sed", "-f" });
        run.addFileArg(upstream.path("bfd/targmatch.sed"));
        run.addFileArg(upstream.path("bfd/config.bfd"));
        _ = bfd_generated.addCopyFile(run.captureStdOut(.{}), "targmatch.h");
    }
    const bfd_include = bfd_generated.getDirectory();

    const flags = [_][]const u8{ "-w", "-fno-strict-aliasing", "-DHAVE_CONFIG_H" };

    // libiberty.
    const iberty = library(b, target, optimize, "iberty");
    iberty.root_module.addIncludePath(config.dir(b, host, "libiberty"));
    iberty.root_module.addIncludePath(include);
    iberty.root_module.addCSourceFiles(.{
        .root = upstream.path("libiberty"),
        .files = if (windows) &(libiberty_sources ++ libiberty_windows) else if (target.result.os.tag == .macos) &(libiberty_sources ++ libiberty_macos) else &(libiberty_sources ++ libiberty_linux),
        .flags = &(flags ++ [_][]const u8{"-D_GNU_SOURCE"}),
    });

    // zlib, as binutils carries it.
    const zlib = library(b, target, optimize, "z");
    zlib.root_module.addCSourceFiles(.{
        .root = upstream.path("zlib"),
        .files = &.{
            "adler32.c", "compress.c", "crc32.c",   "deflate.c", "gzclose.c", "gzlib.c",  "gzread.c",
            "gzwrite.c", "infback.c",  "inffast.c", "inflate.c", "inftrees.c", "trees.c", "uncompr.c",
            "zutil.c",
        },
        .flags = if (windows) &.{ "-w", "-DSTDC_HEADERS=1", "-DHAVE_UNISTD_H=1", "-DHAVE_STDLIB_H=1", "-DHAVE_STRING_H=1", "-DHAVE_MEMCPY=1", "-DHAVE_STRERROR=1" } else &.{ "-w", "-DSTDC_HEADERS=1", "-DHAVE_UNISTD_H=1", "-DHAVE_STDLIB_H=1", "-DHAVE_STRING_H=1", "-DHAVE_MEMCPY=1", "-DHAVE_STRERROR=1", "-DHAVE_GETPAGESIZE=1", "-DHAVE_DLFCN_H=1" },
    });

    // libsframe.
    const sframe = library(b, target, optimize, "sframe");
    sframe.root_module.addIncludePath(config.dir(b, host, "libsframe"));
    sframe.root_module.addIncludePath(include);
    sframe.root_module.addIncludePath(upstream.path("libctf"));
    sframe.root_module.addCSourceFiles(.{
        .root = upstream.path("libsframe"),
        .files = &.{ "sframe.c", "sframe-dump.c", "sframe-error.c" },
        .flags = &flags,
    });

    // BFD, for the target's formats and those every BFD reads.
    const bfd = library(b, target, optimize, "bfd");
    for ([_]std.Build.LazyPath{ config.dir(b, host, "bfd"), bfd_include, upstream.path("bfd"), include, upstream.path("zlib") }) |dir| {
        bfd.root_module.addIncludePath(dir);
    }
    var bfd_flags: std.ArrayList([]const u8) = .empty;
    bfd_flags.appendSlice(b.allocator, &flags) catch @panic("OOM");
    bfd_flags.appendSlice(b.allocator, &.{
        "-DBINDIR=\"" ++ prefix ++ "/bin\"",
        "-DLIBDIR=\"" ++ prefix ++ "/lib\"",
        // What configure adds for an ELF target (`TDEFINES`).
        "-DOBJ_MAYBE_ELF_ATTRIBUTES",
        "-DOBJ_MAYBE_ELF",
    }) catch @panic("OOM");
    for (vectors) |vector| bfd_flags.append(b.allocator, b.fmt("-DHAVE_{s}", .{vector})) catch @panic("OOM");
    bfd.root_module.addCSourceFiles(.{ .root = upstream.path("bfd"), .files = &bfd_sources, .flags = bfd_flags.items });
    var dwarf2_flags: std.ArrayList([]const u8) = .empty;
    dwarf2_flags.appendSlice(b.allocator, bfd_flags.items) catch @panic("OOM");
    dwarf2_flags.append(b.allocator, "-DDEBUGDIR=\"" ++ prefix ++ "/lib/debug\"") catch @panic("OOM");
    bfd.root_module.addCSourceFile(.{ .file = upstream.path("bfd/dwarf2.c"), .flags = dwarf2_flags.items });
    var select: std.ArrayList(u8) = .empty;
    for (vectors, 0..) |vector, i| {
        if (i > 0) select.append(b.allocator, ',') catch @panic("OOM");
        select.print(b.allocator, "&{s}", .{vector}) catch @panic("OOM");
    }
    var defaults: std.ArrayList([]const u8) = .empty;
    defaults.appendSlice(b.allocator, bfd_flags.items) catch @panic("OOM");
    defaults.appendSlice(b.allocator, &.{
        b.fmt("-DDEFAULT_VECTOR={s}", .{vectors[0]}),
        b.fmt("-DSELECT_VECS={s}", .{select.items}),
        "-DSELECT_ARCHITECTURES=&bfd_mips_arch",
    }) catch @panic("OOM");
    bfd.root_module.addCSourceFiles(.{ .root = upstream.path("bfd"), .files = &.{ "targets.c", "archures.c" }, .flags = defaults.items });

    // The MIPS disassembler and opcode tables.
    const opcodes = library(b, target, optimize, "opcodes");
    for ([_]std.Build.LazyPath{ config.dir(b, host, "opcodes"), upstream.path("opcodes"), bfd_include, include, upstream.path("bfd") }) |dir| {
        opcodes.root_module.addIncludePath(dir);
    }
    opcodes.root_module.addCSourceFiles(.{
        .root = upstream.path("opcodes"),
        .files = &.{ "dis-buf.c", "dis-init.c", "disassemble.c", "mips-dis.c", "mips-opc.c", "mips16-opc.c", "micromips-opc.c" },
        .flags = &(flags ++ [_][]const u8{"-DARCH_mips"}),
    });

    const libraries = [_]*std.Build.Step.Compile{ bfd, opcodes, sframe, iberty, zlib };

    // gas.
    const gas_generated = b.addWriteFiles();
    _ = gas_generated.addCopyFile(b.path(b.fmt("hosts/{s}/gas.h", .{host})), "config.h");
    _ = gas_generated.add("targ-cpu.h", "#include \"tc-mips.h\"\n");
    _ = gas_generated.add("obj-format.h", "#include \"obj-elf.h\"\n");
    _ = gas_generated.add("targ-env.h", "#include \"te-tmips.h\"\n");
    _ = gas_generated.add("itbl-cpu.h", "#include \"itbl-mips.h\"\n");
    const as = program(b, target, optimize, "as", &libraries);
    for ([_]std.Build.LazyPath{ gas_generated.getDirectory(), upstream.path("gas"), bfd_include, upstream.path("gas/config"), include, src, upstream.path("bfd"), upstream.path("zlib") }) |dir| {
        as.root_module.addIncludePath(dir);
    }
    as.root_module.addCSourceFiles(.{
        .root = upstream.path("gas"),
        .files = &gas_sources,
        .flags = &(flags ++ [_][]const u8{"-DLOCALEDIR=\"" ++ prefix ++ "/share/locale\""}),
    });

    // ld, its emulations made by its own scripts.
    const ld_generated = b.addWriteFiles();
    _ = ld_generated.addCopyFile(b.path(b.fmt("hosts/{s}/ld.h", .{host})), "config.h");
    var list: std.ArrayList(u8) = .empty;
    list.appendSlice(b.allocator, "/* This file is automatically generated.  DO NOT EDIT! */\n") catch @panic("OOM");
    for (emulations) |e| list.print(b.allocator, "extern ld_emulation_xfer_type ld_{s}_emulation;\n", .{e}) catch @panic("OOM");
    list.appendSlice(b.allocator, "#define EMULATION_LIST \\\n") catch @panic("OOM");
    for (emulations) |e| list.print(b.allocator, "  &ld_{s}_emulation, \\\n", .{e}) catch @panic("OOM");
    list.appendSlice(b.allocator, "  0\n") catch @panic("OOM");
    _ = ld_generated.add("ldemul-list.h", list.items);

    const ld = program(b, target, optimize, "ld", &libraries);
    for ([_]std.Build.LazyPath{ ld_generated.getDirectory(), upstream.path("ld"), bfd_include, upstream.path("bfd"), include, upstream.path("zlib") }) |dir| {
        ld.root_module.addIncludePath(dir);
    }
    const ld_flags = flags ++ [_][]const u8{
        "-DLOCALEDIR=\"" ++ prefix ++ "/share/locale\"",
        "-DELF_LIST_OPTIONS=true",
        "-DELF_SHLIB_LIST_OPTIONS=true",
        "-DELF_PLT_UNWIND_LIST_OPTIONS=false",
        "-DELF_SFRAME_LIST_OPTIONS=false",
    };
    const tooldir = prefix ++ "/" ++ target_alias;
    ld.root_module.addCSourceFiles(.{ .root = upstream.path("ld"), .files = &ld_sources, .flags = &ld_flags });
    ld.root_module.addCSourceFile(.{ .file = upstream.path("ld/ldmain.c"), .flags = &(ld_flags ++ [_][]const u8{
        "-DDEFAULT_EMULATION=\"" ++ emulations[0] ++ "\"",
        "-DBINDIR=\"" ++ prefix ++ "/bin\"",
        "-DTOOLBINDIR=\"" ++ tooldir ++ "/bin\"",
        "-DTARGET=\"" ++ target_canonical ++ "\"",
        "-DTARGET_SYSTEM_ROOT=\"\"",
    }) });
    ld.root_module.addCSourceFile(.{ .file = upstream.path("ld/ldfile.c"), .flags = &(ld_flags ++ [_][]const u8{
        "-DSCRIPTDIR=\"" ++ tooldir ++ "/lib\"",
        "-DBINDIR=\"" ++ prefix ++ "/bin\"",
        "-DTOOLBINDIR=\"" ++ tooldir ++ "/bin\"",
    }) });
    var libpath: std.ArrayList(u8) = .empty;
    for (emulations, 0..) |e, i| {
        if (i > 0) libpath.append(b.allocator, ' ') catch @panic("OOM");
        libpath.appendSlice(b.allocator, e) catch @panic("OOM");
    }
    for (emulations) |e| {
        // Run in the folder it writes into, given the script and ld's sources there as paths the shell makes
        // absolute. The emulation's source starts with the name it has in binutils' own build (`#line`), which its
        // assertions say: the folder it is written into is named by a hash of the step, which zig makes of where the
        // files it reads are, and would name ld after where the project is.
        const run = b.addRunArtifact(sh);
        run.addArgs(&.{ "-c", b.fmt(
            "o=$1; s=$PWD/$2; d=$PWD/$3; shift 3; cd \"$o\" && mkdir ldscripts && ( . \"$s\" \"$d\" \"$@\" ) && " ++
                "{{ echo '#line 1 \"e{s}.c\"'; cat e{s}.c; }} > e{s}.named.c",
            .{ e, e, e },
        ), "sh" });
        const out = run.addOutputDirectoryArg(b.fmt("e{s}", .{e}));
        run.addFileArg(upstream.path("ld/genscripts.sh"));
        run.addDirectoryArg(upstream.path("ld"));
        // libdir, prefix, exec_prefix, host, target, its alias, the dependency folder (none), the library path (none
        // given), the emulations with library paths, the native library folders, a sysroot (none), init/fini arrays.
        run.addArgs(&.{ prefix ++ "/lib", prefix, prefix, "build", target_canonical, target_alias, "", "", libpath.items, "/usr/local/lib /lib /usr/lib", "no", "yes", e, target_alias });
        run.expectExitCode(0);
        ld.root_module.addCSourceFile(.{ .file = out.path(b, b.fmt("e{s}.named.c", .{e})), .flags = &ld_flags });
    }

    // nm and objcopy.
    const binutils_config = config.dir(b, host, "binutils");
    const binutils_flags = flags ++ [_][]const u8{
        "-DLOCALEDIR=\"" ++ prefix ++ "/share/locale\"",
        "-Dbin_dummy_emulation=bin_vanilla_emulation",
    };
    const bulibs = [_][]const u8{ "bucomm.c", "version.c", "filemode.c" };
    for ([_]struct { []const u8, []const []const u8 }{
        .{ "nm", &(.{ "nm.c", "demanguse.c" } ++ bulibs) },
        .{ "objcopy", &(.{ "not-strip.c", "rename.c", "rddbg.c", "debug.c", "stabs.c", "rdcoff.c", "wrstabs.c" } ++ bulibs) },
    }) |p| {
        const exe = program(b, target, optimize, p[0], &libraries);
        for ([_]std.Build.LazyPath{ binutils_config, upstream.path("binutils"), bfd_include, upstream.path("bfd"), include, upstream.path("zlib") }) |dir| {
            exe.root_module.addIncludePath(dir);
        }
        exe.root_module.addCSourceFiles(.{ .root = upstream.path("binutils"), .files = p[1], .flags = &binutils_flags });
    }
}

fn library(b: *std.Build, target: std.Build.ResolvedTarget, optimize: std.builtin.OptimizeMode, name: []const u8) *std.Build.Step.Compile {
    return b.addLibrary(.{
        .name = name,
        .linkage = .static,
        .root_module = b.createModule(.{
            .target = target,
            .optimize = optimize,
            .link_libc = true,
            .sanitize_c = .off,
        }),
    });
}

fn program(
    b: *std.Build,
    target: std.Build.ResolvedTarget,
    optimize: std.builtin.OptimizeMode,
    name: []const u8,
    libraries: []const *std.Build.Step.Compile,
) *std.Build.Step.Compile {
    const exe = b.addExecutable(.{
        .name = name,
        .root_module = b.createModule(.{
            .target = target,
            .optimize = optimize,
            .link_libc = true,
            .strip = true,
            .sanitize_c = .off,
        }),
    });
    for (libraries) |l| exe.root_module.linkLibrary(l);
    b.installArtifact(exe);
    return exe;
}

const libiberty_sources = [_][]const u8{
    "alloca.c",            "argv.c",              "bsearch_r.c",          "choose-temp.c",       "concat.c",
    "cp-demangle.c",       "cp-demint.c",         "cplus-dem.c",          "crc32.c",             "d-demangle.c",
    "dwarfnames.c",        "dyn-string.c",        "fdmatch.c",            "fibheap.c",           "filedescriptor.c",
    "filename_cmp.c",      "floatformat.c",       "fnmatch.c",            "fopen_unlocked.c",    "getopt.c",
    "getopt1.c",           "getpwd.c",            "getruntime.c",         "hashtab.c",           "hex.c",
    "lbasename.c",         "ldirname.c",          "lrealpath.c",          "make-relative-prefix.c", "make-temp-file.c",
    "md5.c",               "objalloc.c",          "obstack.c",            "partition.c",         "pex-common.c",
    "pex-one.c",           "pexecute.c",          "physmem.c",            "regex.c",             "rust-demangle.c",
    "safe-ctype.c",        "setproctitle.c",      "sha1.c",               "simple-object.c",     "simple-object-coff.c",
    "simple-object-elf.c", "simple-object-mach-o.c", "simple-object-xcoff.c", "sort.c",           "spaces.c",
    "splay-tree.c",        "stack-limit.c",       "strerror.c",           "strsignal.c",         "timeval-utils.c",
    "unlink-if-ordinary.c", "vprintf-support.c",  "xasprintf.c",          "xatexit.c",           "xexit.c",
    "xmalloc.c",           "xmemdup.c",           "xstrdup.c",            "xstrerror.c",         "xstrndup.c",
    "xvasprintf.c",
};
/// What each host's C library lacks, by configure's answers.
const libiberty_linux = [_][]const u8{ "pex-unix.c", "sigsetmask.c" };
const libiberty_macos = [_][]const u8{ "pex-unix.c", "memrchr.c", "mempcpy.c", "strverscmp.c" };
const libiberty_windows = [_][]const u8{
    "pex-win32.c", "bcopy.c",     "bzero.c",     "ffs.c",     "getpagesize.c", "index.c",  "insque.c",
    "memmem.c",    "memrchr.c",   "mkstemps.c",  "random.c",  "rindex.c",      "setenv.c", "sigsetmask.c",
    "stpcpy.c",    "stpncpy.c",   "strverscmp.c", "vfork.c",  "waitpid.c",
};

const bfd_sources = [_][]const u8{
    "archive.c",   "bfd.c",         "bfdio.c",     "binary.c",   "cache.c",        "coff-bfd.c",     "compress.c",
    "corefile.c",  "format.c",      "hash.c",      "ihex.c",     "libbfd.c",       "linker.c",       "merge.c",
    "opncls.c",    "reloc.c",       "section.c",   "simple.c",   "srec.c",         "stab-syms.c",    "stabs.c",
    "syms.c",      "tekhex.c",      "verilog.c",   "archive64.c",
    // The target's back ends and machine.
    "elf32-mips.c", "elfxx-mips.c", "elf32.c",     "elf-attrs.c", "elf.c",         "elflink.c",      "elf-strtab.c",
    "elf-eh-frame.c", "elf-properties.c", "dwarf1.c", "ecofflink.c",  "coffgen.c",      "coff-mips.c",
    "ecoff.c",     "elfn32-mips.c", "elf64-mips.c", "elf64.c",   "elf64-gen.c",    "elf32-gen.c",    "cpu-mips.c",
};

const gas_sources = [_][]const u8{
    "app.c",           "as.c",            "atof-generic.c",  "codeview.c",      "compress-debug.c", "cond.c",
    "depend.c",        "dwarf2dbg.c",     "dw2gencfi.c",     "ecoff.c",         "ehopt.c",          "expr.c",
    "flonum-copy.c",   "flonum-konst.c",  "flonum-mult.c",   "frags.c",         "gen-sframe.c",     "ginsn.c",
    "hash.c",          "input-file.c",    "input-scrub.c",   "listing.c",       "literal.c",        "macro.c",
    "messages.c",      "output-file.c",   "read.c",          "remap.c",         "sb.c",             "scfidw2gen.c",
    "scfi.c",          "sframe-opt.c",    "stabs.c",         "subsegs.c",       "symbols.c",        "write.c",
    "config/tc-mips.c", "config/obj-elf.c", "config/atof-ieee.c", "config/obj-elf-attr.c", "itbl-parse.c",
    "itbl-lex-wrapper.c", "itbl-ops.c",   "config/e-mipself.c",
};

const ld_sources = [_][]const u8{
    "ldgram.c",  "ldlex-wrapper.c", "lexsup.c", "ldlang.c", "mri.c",    "ldctor.c", "ldwrite.c",
    "ldexp.c",   "ldemul.c",        "ldver.c",  "ldmisc.c", "ldcref.c", "plugin.c", "ldbuildid.c",
    "ldelf.c",   "ldelfgen.c",
};
