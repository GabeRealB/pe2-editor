# CPython's port builds it by hand from what its own build does for each host: its configure's answers (`hosts/<host>.h`,
# its pyconfig.h), the sources each host's build compiles and how (`hosts/sources.zon`), and the modules it builds in
# (`hosts/modules.zon`) - written by `cpython.py` from a reference build of each, configured as the port builds it (every
# module that needs nothing outside CPython built in) and made. Windows' is MSYS2's CPython for MinGW, whose configure
# is made first (autoreconf); Linux's is static musl, as the package is. A cross build of CPython needs a CPython of the
# same version for the machine building, made first, natively.
PORT=cpython-3.14

run() {
  local upstream=$WORK/src/cpython mingw=$WORK/src/cpython-mingw ref=$WORK/build/cpython-ref
  unpack "$(url_of $PORT python)" "$upstream"
  # As the port mends it for macOS without Apple's SDK (and nothing changes for Linux's).
  patch -s -p1 -d "$upstream" < "$PACKAGES/ports/$PORT/patches/macos-system-log.patch" || { echo "cpython: its patch does not apply"; exit 1; }
  unpack "$(url_of $PORT python_mingw)" "$mingw"
  (cd "$mingw" && PATH=$AUTOTOOLS_BIN:$PATH ACLOCAL_PATH=$TOOLS/autotools/share/aclocal:/usr/share/aclocal autoreconf -if > autoreconf.log 2>&1) \
    || { echo "cpython: MinGW's configure is not made (see $mingw/autoreconf.log)"; exit 1; }
  if [ ! -x "$ref/python" ]; then
    rm -rf "$ref"; mkdir -p "$ref"
    # zig's clang refuses __DATE__ by default, which CPython's build information reads.
    (cd "$ref" && CC="$ZIG cc -Wno-date-time" "$upstream/configure" --disable-shared --with-ensurepip=no --disable-test-modules --without-mimalloc \
      > configure.log 2>&1 && make -j$JOBS > make.log 2>&1) || { echo "cpython: the reference Python is not made (see $ref)"; exit 1; }
  fi
  for host in $HOSTS; do
    . "$HERE/env.sh" $host musl
    local b=$WORK/build/cpython-$host exe="" src=$upstream
    [ $host = windows ] && { exe=.exe; src=$mingw; }
    rm -rf "$b"; mkdir -p "$b"; cd "$b"
    # What configure would ask of the machine it runs on, which is not the one built for.
    printf 'ac_cv_file__dev_ptmx=no\nac_cv_file__dev_ptc=no\nac_cv_buggy_getaddrinfo=no\n' > config.site
    CONFIG_SITE=$b/config.site CFLAGS="-O2 -w" "$src/configure" --host=$TRIPLE --build=x86_64-pc-linux-gnu --disable-shared \
      --with-ensurepip=no --disable-test-modules --without-mimalloc --without-remote-debug --with-pkg-config=no \
      --with-build-python=$ref/python > configure.log 2>&1 || { echo "cpython $host: configure failed (see $b/configure.log)"; exit 1; }
    # Every module built in, but those whose headers no host here has (remote debugging, Apple's proxy settings,
    # Windows' WMI, which is C++ against COM headers).
    sed -i 's/^\*shared\*/*static*/; s/^_remote_debugging /#_remote_debugging /; s/^_scproxy /#_scproxy /; s/^_wmi /#_wmi /' Modules/Setup.stdlib
    rm -f Modules/Setup.local
    make Makefile > /dev/null 2>&1
    # What zig links: Windows' path functions from the API set, not pathcch; no Apple framework; the modules' libraries
    # static.
    sed -i 's/-lpathcch/-lapi-ms-win-core-path-l1-1-0/g; s/-framework CoreFoundation//g; /^MODULE__[A-Z0-9_]*_\(LDFLAGS\|DEPS\)=/s/_LIB_SHARED)/_LIB_STATIC)/g' Makefile
    # They link, but the C library headers zig carries for macOS declare none of them.
    [ $host = macos ] && sed -i -E 's/^#define (HAVE_OPENPTY|HAVE_LOGIN_TTY|HAVE_FORKPTY|HAVE_SENDFILE) 1/\/* #undef \1 *\//' pyconfig.h
    # Windows' program resource, which windres would make: no windres here, and the port builds it its own way.
    echo "" > empty.c; $CC -c empty.c -o python_exe.o
    make -j$JOBS -O python$exe > make.log 2>&1 || { echo "cpython $host: make failed (see $b/make.log)"; exit 1; }
  done
  python3 "$HERE/cpython.py" "$WORK" "$upstream" "$mingw" "$PACKAGES/ports/$PORT" $HOSTS
}
