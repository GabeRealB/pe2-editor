# . env.sh <host: linux|macos|windows> [musl]: what a library's own build runs with for <host> - on Linux for glibc
# 2.28, as FFmpeg's libraries are, or with `musl` static musl, as the toolchain packages are: CC, CXX, AR, RANLIB, NM and
# PATH (the wrappers first); $TARGET (zig's), $TRIPLE (autotools' --host); $SYS, where the libraries harvested before it
# are installed for it to find (include/, lib/, lib/pkgconfig/, PKG_CONFIG_LIBDIR); $TOOLCHAIN, CMake's toolchain file;
# $CROSS, meson's cross file.

HOST=$1
case $HOST in
  linux) TARGET=x86_64-linux-gnu.2.28; TRIPLE=x86_64-linux-gnu; SYSTEM=Linux; CPU=x86_64; MESON_SYSTEM=linux; MESON_CPU=x86_64 ;;
  macos) TARGET=aarch64-macos; TRIPLE=aarch64-apple-darwin; SYSTEM=Darwin; CPU=arm64; MESON_SYSTEM=darwin; MESON_CPU=aarch64 ;;
  windows) TARGET=x86_64-windows-gnu; TRIPLE=x86_64-w64-mingw32; SYSTEM=Windows; CPU=AMD64; MESON_SYSTEM=windows; MESON_CPU=x86_64 ;;
  *) echo "no host $HOST" >&2; return 1 ;;
esac
if [ "$2" = musl ] && [ $HOST = linux ]; then TARGET=x86_64-linux-musl; TRIPLE=x86_64-pc-linux-musl; fi
export CC=$TOOLS/bin/cc-$TARGET CXX=$TOOLS/bin/c++-$TARGET AR=$TOOLS/bin/ar RANLIB=$TOOLS/bin/ranlib
export NM=$( [ $HOST = macos ] && echo $TOOLS/bin/nm-macos || echo nm )
export PATH=$TOOLS/bin:$PATH
export SYS=$WORK/sys-$HOST$( [ $TARGET = x86_64-linux-musl ] && echo -musl )
export PKG_CONFIG_LIBDIR=$SYS/lib/pkgconfig PKG_CONFIG_PATH= PKG_CONFIG_SYSROOT_DIR=
mkdir -p $SYS/include $SYS/lib/pkgconfig

TOOLCHAIN=$WORK/toolchain-$HOST.cmake
cat > $TOOLCHAIN <<EOF
set(CMAKE_SYSTEM_NAME $SYSTEM)
set(CMAKE_SYSTEM_PROCESSOR $CPU)
set(CMAKE_C_COMPILER $CC)
set(CMAKE_CXX_COMPILER $CXX)
set(CMAKE_ASM_NASM_COMPILER $TOOLS/bin/nasm)
set(CMAKE_AR $AR)
set(CMAKE_RANLIB $RANLIB)
set(CMAKE_FIND_ROOT_PATH $SYS)
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
EOF
# Apple's install_name_tool, which CMake asks for on Darwin, and a static build never runs.
[ $HOST = macos ] && echo "set(CMAKE_INSTALL_NAME_TOOL /bin/true)" >> $TOOLCHAIN

CROSS=$WORK/cross-$HOST.ini
cat > $CROSS <<EOF
[binaries]
c = '$CC'
cpp = '$CXX'
ar = '$AR'
ranlib = '$RANLIB'
nasm = '$TOOLS/bin/nasm'
pkg-config = 'pkg-config'
strip = 'true'
[properties]
pkg_config_libdir = '$SYS/lib/pkgconfig'
[host_machine]
system = '$MESON_SYSTEM'
cpu_family = '$MESON_CPU'
cpu = '$MESON_CPU'
endian = 'little'
EOF
