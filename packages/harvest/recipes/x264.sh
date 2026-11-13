# x264's port is written by hand; its configure gives what it decides of each host (`hosts/<host>-config.h`,
# `-x264_config.h`). The port, built, is installed for FFmpeg's configure.
PORT=x264
run() {
  local url; url=$(url_of x264 x264)
  for host in $HOSTS; do
    . "$HERE/env.sh" $host
    local src=$WORK/src/x264-$host b=$WORK/build/x264-$host
    unpack "$url" "$src"; rm -rf "$b"; mkdir -p "$b"
    (cd "$b" && AS=$( [ $host = macos ] && echo $CC || echo $TOOLS/bin/nasm ) "$src/configure" --host=$TRIPLE \
      --enable-static --enable-pic --disable-cli --disable-opencl > configure.log 2>&1) || { echo "x264 $host: configure failed"; exit 1; }
    for f in config.h x264_config.h; do
      { printf '/* %s for %s: what x264'"'"'s configure decided for that host (`--enable-static --enable-pic --disable-cli --disable-opencl`, all bit\n   depths), run with zig'"'"'s C compiler. */\n' $f $host; cat "$b/$f"; } > "$PACKAGES/ports/x264/hosts/$host-$f"
    done
    zig_install "$PACKAGES/ports/x264" "$b/port" || exit 1
    # Its libraries as one, as configure links it (the port's further libraries hold sources of a name it has).
    cp -r "$b/port/include/." "$SYS/include/"
    { echo "create $SYS/lib/libx264.a"; for l in "$b"/port/lib/*x264*; do echo "addlib $l"; done; echo save; echo end; } | "$ZIG" ar -M
    sys_pc x264 0.165 -lx264 "$( [ $host = linux ] && echo '-lpthread -lm' || { [ $host = macos ] && echo -lm; } )"
    echo "  $host: hosts/$host-config.h, hosts/$host-x264_config.h"
  done
}
