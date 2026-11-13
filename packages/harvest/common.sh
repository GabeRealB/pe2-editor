# What recipes share: the configure, make and install of a CMake, autotools or meson build, installed into $SYS;
# and what reads the ports' manifests.

cmake_configure() { cmake -G Ninja ${CMAKE_SOURCE:-$SRC} -DCMAKE_TOOLCHAIN_FILE=$TOOLCHAIN -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=$SYS -DBUILD_SHARED_LIBS=OFF -DCMAKE_POLICY_VERSION_MINIMUM=3.5 "$@"; }
cmake_make() { ninja -j$JOBS -v; }
cmake_install() { cmake --install . --prefix $SYS; }

autotools_configure() { CPPFLAGS=-I$SYS/include LDFLAGS=-L$SYS/lib $SRC/configure --host=$TRIPLE --prefix=$SYS --enable-static --disable-shared "$@"; }
autotools_make() { make -j$JOBS V=1; }
autotools_install() { make install; }

meson_configure() { $MESON setup --cross-file $CROSS --prefix $SYS --buildtype release --default-library static -Db_ndebug=true "$@" . $SRC; }
meson_make() { ninja -j$JOBS -v; }
meson_install() { $MESON install --no-rebuild; }

# url_of <port> <dependency>: the URL a port's manifest fetches a dependency from.
url_of() {
  python3 - "$PACKAGES/ports/$1/build.zig.zon" "$2" <<'EOF'
import re, sys
text = open(sys.argv[1]).read()
m = re.search(r"\.%s = \.\{\s*\.url = \"([^\"]+)\"" % re.escape(sys.argv[2]), text)
if m is None:
    sys.exit(f"{sys.argv[1]} fetches no {sys.argv[2]}")
print(m.group(1))
EOF
}

# unpack <url> <folder>: a source fetched as zig fetches it - an archive's single top folder taken as its root, a
# repository's tree at the commit after `#` - into <folder>.
unpack() {
  local url=$1 out=$2 archive
  rm -rf "$out" "$out.unpack"; mkdir -p "$out.unpack"
  case $url in
    git+*)
      local repo=${url#git+} ref=${url##*#}; repo=${repo%%#*}; repo=${repo%%\?*}
      archive=$WORK/downloads/$(basename "$repo" .git)-$ref.tar
      if [ ! -f "$archive" ]; then
        rm -rf "$archive.git" && git clone -q --filter=blob:none "$repo" "$archive.git" \
          && git -C "$archive.git" archive --prefix=tree/ -o "$archive" "$ref" && rm -rf "$archive.git" || return 1
      fi ;;
    *)
      # Named by its URL's last two parts: GitHub's tag archives share their names (v1.6.0.tar.gz).
      local name; name=$(echo "$url" | awk -F/ '{print $(NF-1) "-" $NF}')
      download "$url" "$name" || return 1
      archive=$WORK/downloads/$name ;;
  esac
  case $archive in *.zip) unzip -q "$archive" -d "$out.unpack" ;; *) tar xf "$archive" -C "$out.unpack" ;; esac
  local root=$out.unpack; set -- $root/*; [ $# = 1 ] && [ -d "$1" ] && root=$1
  mv "$root" "$out"; rm -rf "$out.unpack"
}

# zig_install <folder> <prefix> [option...]: a zig package (a port, or one of allyourcodebase's) built for $TARGET
# into <prefix>.
zig_install() {
  local dir=$1 prefix=$2; shift 2
  (cd "$dir" && $ZIG build -Dtarget=$TARGET -Doptimize=ReleaseFast --prefix "$prefix" "$@")
}

# sys_library <file> <name>: a static library put in $SYS as lib<name>.a, the name configure scripts link by (zig names
# Windows' <name>.lib).
sys_library() { cp "$1" "$SYS/lib/lib$2.a"; }

# sys_pc <name> <version> <libs> [private libs] [cflags]: a .pc file for a library installed in $SYS without one.
sys_pc() {
  printf 'prefix=%s\nlibdir=${prefix}/lib\nincludedir=${prefix}/include\n\nName: %s\nDescription: %s\nVersion: %s\nLibs: -L${libdir} %s\nLibs.private: %s\nCflags: -I${includedir} %s\n' \
    "$SYS" "$1" "$1" "$2" "$3" "$4" "$5" > "$SYS/lib/pkgconfig/$1.pc"
}

# allyourcodebase <dependency> <library> <name> [zig option...]: one of allyourcodebase's ports, as FFmpeg's port
# fetches it, built for each host and its library put in $SYS as lib<name>.a with its headers; the folder it was
# installed in is $PREFIX of the last host.
allyourcodebase() {
  local dep=$1 lib=$2 name=$3; shift 3
  local src=$WORK/src/$dep
  unpack "$(url_of ffmpeg-9.0.2 $dep)" "$src"
  for host in $HOSTS; do
    . "$HERE/env.sh" $host
    PREFIX=$WORK/build/$dep-$host; rm -rf "$PREFIX"
    zig_install "$src" "$PREFIX" "$@" > "$WORK/build/$dep-$host.log" 2>&1 || { echo "$dep $host does not build"; exit 1; }
    cp -r "$PREFIX/include/." "$SYS/include/"
    sys_library "$(ls "$PREFIX"/lib/*$lib* | head -1)" $name
    allyourcodebase_installed
  done
}
allyourcodebase_installed() { :; }
