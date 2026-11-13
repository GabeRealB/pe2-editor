# binutils' port builds its parts (libiberty, libsframe, BFD, opcodes, gas, ld, binutils) by hand; each part's configure
# gives what it decides of each host, its `config.h` (`hosts/<host>/<part>.h`), configured for a mipsel target. On Linux
# for static musl, as the package is.
PORT=binutils-2.47
PARTS="libiberty libsframe bfd opcodes gas ld binutils"

run() {
  local src=$WORK/src/binutils
  unpack "$(url_of $PORT binutils)" "$src"
  for host in $HOSTS; do
    . "$HERE/env.sh" $host musl
    local b=$WORK/build/binutils-$host what
    case $host in linux) what="Linux, with musl" ;; macos) what=macOS ;; windows) what="Windows, with MinGW-w64" ;; esac
    rm -rf "$b"; mkdir -p "$b"
    for part in $PARTS; do
      mkdir -p "$b/$part"
      (cd "$b/$part" && CFLAGS="-O2 -w" "$src/$part/configure" --host=$TRIPLE --build=x86_64-pc-linux-gnu \
        --target=mipsel-linux-gnu --disable-nls --disable-werror --disable-plugins --without-zstd --disable-libctf \
        --without-debuginfod --without-msgpack --enable-targets=mipsel-linux-gnu > configure.log 2>&1) \
        || { echo "binutils $host $part: configure failed (see $b/$part/configure.log)"; exit 1; }
    done
    python3 - "$b" "$PACKAGES/ports/$PORT/hosts/$host" "$what" "$TARGET" $PARTS <<'PY'
import os, re, sys
build, out, what, target, parts = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5:]
os.makedirs(out, exist_ok=True)
for part in parts:
    # Without its comments, a blank line kept where any ran, under a header saying what it is.
    text = re.sub(r"/\*.*?\*/", "", open(f"{build}/{part}/config.h").read(), flags=re.S)
    lines, blank = [], False
    for line in (l.rstrip() for l in text.split("\n")):
        if not line.strip():
            blank = True
            continue
        if blank and lines:
            lines.append("")
        blank = False
        lines.append(line)
    header = (f"/* {part}/config.h for {what}: what binutils 2.47's configure decided for that host (`--target=mipsel-linux-gnu`,\n"
              f"   without NLS, plugins, zstd or libctf), run with zig's C compiler for {target}. */\n\n")
    open(f"{out}/{part}.h", "w").write(header + "\n".join(lines) + "\n")
PY
    echo "  $host: hosts/$host/{$(echo $PARTS | tr ' ' ',')}.h"
  done
}
