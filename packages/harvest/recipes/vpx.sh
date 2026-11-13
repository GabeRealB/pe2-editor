URL=https://github.com/webmproject/libvpx/archive/refs/tags/v1.17.0.tar.gz
PORT=vpx; ARCHIVES=libvpx_g.a=vpx
vpx_target() { case $HOST in linux) echo x86_64-linux-gcc ;; windows) echo x86_64-win64-gcc ;; macos) echo arm64-darwin20-gcc ;; esac; }
configure() { LD=$CC $SRC/configure --target=$(vpx_target) --prefix=$SYS --as=nasm --disable-examples --disable-tools --disable-docs --disable-unit-tests --enable-vp9-highbitdepth --enable-runtime-cpu-detect --enable-pic --disable-shared --enable-static; }
make_it() { make -j$JOBS V=1 libvpx.a; }
# libvpx_g.a: libvpx.a is that archive put through this machine's strip, which writes Mach-O archives in GNU's format.
install() { make install-headers install-pcs >/dev/null 2>&1 || make install; cp libvpx_g.a $SYS/lib/libvpx.a; }
