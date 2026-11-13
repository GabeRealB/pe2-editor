# . tools.sh: the tools a harvest runs libraries' own builds with, set up once in $WORK/tools.
#
# - wrappers for zig's compilers, nasm and zig's ar (`$WORK/tools/bin`): each runs the real tool and, while
#   $HARVEST_LOG is set, first appends a record of how it was run to it - a byte 0x1e, then the tool's name, the
#   folder it runs in and its arguments, each ended by 0x1f - under a lock, as a long record is written in pieces
#   which a parallel run's would otherwise split;
# - nasm, allyourcodebase's port, as the ports use it (`ports/harvested`), built for this machine;
# - meson, from PyPI, in a virtual environment of its own;
# - GNU autoconf, automake, autoconf-archive and libtool, for the libraries whose releases have no configure script.
#
# Each is pinned here, and each download held to its SHA-256.

AUTOTOOLS="autoconf/autoconf-2.72 ba885c1319578d6c94d46e9b0dceb4014caafe2490e437a0dbca3f270a223f5a
automake/automake-1.17 8920c1fc411e13b90bf704ef9db6f29d540e76d232cb3b2c9f4dc4cc599bd990
autoconf-archive/autoconf-archive-2024.10.16 7bcd5d001916f3a50ed7436f4f700e3d2b1bade3ed803219c592d62502a57363
libtool/libtool-2.5.4 f81f5860666b0bc7d84baddefa60d1cb9fa6fceb2398cc3baca6afaa60266675"
MESON_VERSION=1.12.1
# glibc 2.28 for FFmpeg's libraries, static musl for the toolchain packages' (binutils, CPython).
TARGETS="x86_64-linux-gnu.2.28 x86_64-linux-musl x86_64-windows-gnu aarch64-macos"

TOOLS=$WORK/tools
mkdir -p $TOOLS/bin $WORK/downloads

# download <url> <file> [sha256]: <url> saved as $WORK/downloads/<file>, once; held to <sha256> when given.
download() {
  local out=$WORK/downloads/$2
  if [ ! -f "$out" ]; then
    curl -sfL --retry 3 --max-time 900 -o "$out.part" "$1" && mv "$out.part" "$out" || { echo "cannot download $1" >&2; return 1; }
  fi
  if [ -n "$3" ] && [ "$(sha256sum < "$out" | cut -c1-64)" != "$3" ]; then
    echo "$1 is not what is pinned (SHA-256 $3)" >&2; rm -f "$out"; return 1
  fi
}

# The wrappers.
log='if [ -n "$HARVEST_LOG" ]; then r=$(printf "\036%s\037%s\037" "$TOOL" "$PWD"; printf "%s\037" "$@"); { flock 9; printf "%s" "$r" >&9; } 9>>"$HARVEST_LOG"; fi'
for t in $TARGETS; do
  for k in cc c++; do
    cat > $TOOLS/bin/$k-$t <<EOF
#!/bin/bash
# zig's $k for $t, as the builds harvested run it.
TOOL=$k
args=()
for a in "\$@"; do
  case "\$a" in
    # Link flags zig's linker does not take.
    -Wl,-dynamic,-search_paths_first|-Wl,-search_paths_first|-Wl,-headerpad_max_install_names) ;;
    -Wl,*--pic-executable*|-Wl,*--image-base*) a=\$(echo "\$a" | sed -E 's/,--pic-executable//; s/,--image-base,[^,]*//; s/,-e,mainCRTStartup//'); [ "\$a" != "-Wl" ] && args+=("\$a") ;;
    # A target as clang names it, which zig's driver refuses: this wrapper gives zig's.
    --target=*) ;;
    # An arm64 architecture with extensions (-march=armv8.2-a+dotprod), which zig's driver reads as a CPU: zig's form,
    # its features named as zig names them (sve2_bitperm).
    -march=armv*) a=\$(echo "\${a#-march=arm}" | sed -E 's/^v([0-9]+)\.([0-9]+)-a/-mcpu=generic+v\1_\2a/; s/^v([0-9]+)-a/-mcpu=generic/; s/([a-z0-9])-([a-z])/\1_\2/g'); args+=("\$a") ;;
    *) args+=("\$a") ;;
  esac
done
# Preprocessing asked with -c as well (fontconfig's build), which clang takes as preprocessing and zig's driver as
# compiling.
case " \${args[*]} " in *" -E "*) kept=(); for a in "\${args[@]}"; do [ "\$a" = -c ] || kept+=("\$a"); done; args=("\${kept[@]}") ;; esac
set -- "\${args[@]}"
$log
# __DATE__ and __TIME__, which zig's clang refuses by default, some libraries' builds use.
exec $ZIG $k -target $t -Wno-date-time "\$@"
EOF
    chmod +x $TOOLS/bin/$k-$t
  done
done
for k in ar ranlib; do
  printf '#!/bin/bash\nTOOL=%s\n%s\nexec %s %s "$@"\n' $k "$log" $ZIG $k > $TOOLS/bin/$k; chmod +x $TOOLS/bin/$k
done
# What a configure script asks nm of a Mach-O object: how a C symbol is spelled there, with an underscore.
printf '#!/bin/sh\necho "0000000000000000 T _ff_extern"\n' > $TOOLS/bin/nm-macos; chmod +x $TOOLS/bin/nm-macos

# nasm.
if [ ! -x $TOOLS/nasm/bin/nasm ]; then
  (cd $PACKAGES/ports/harvested && $ZIG build -Dnasm --prefix $TOOLS/nasm) || { echo "nasm does not build" >&2; return 1; }
fi
printf '#!/bin/bash\nTOOL=nasm\n%s\nexec %s "$@"\n' "$log" $TOOLS/nasm/bin/nasm > $TOOLS/bin/nasm; chmod +x $TOOLS/bin/nasm

# meson.
if [ ! -x $TOOLS/meson/bin/meson ]; then
  python3 -m venv $TOOLS/meson && $TOOLS/meson/bin/pip install -q meson==$MESON_VERSION || { echo "meson does not install" >&2; return 1; }
fi
MESON=$TOOLS/meson/bin/meson

# The autotools, each configured and installed into $TOOLS/autotools with those before it on the path.
AUTOTOOLS_BIN=$TOOLS/autotools/bin
if [ ! -x $AUTOTOOLS_BIN/libtoolize ]; then
  echo "$AUTOTOOLS" | while read -r name sha; do
    file=$(basename $name).tar.xz
    download https://ftpmirror.gnu.org/$name.tar.xz $file $sha || return 1
    rm -rf $TOOLS/src/${file%.tar.xz} && mkdir -p $TOOLS/src && tar xf $WORK/downloads/$file -C $TOOLS/src
    (cd $TOOLS/src/${file%.tar.xz} && PATH=$AUTOTOOLS_BIN:$PATH ./configure --prefix=$TOOLS/autotools > configure.log 2>&1 \
      && make -j$JOBS > make.log 2>&1 && make install > install.log 2>&1) || { echo "$name does not build" >&2; return 1; }
  done
fi
