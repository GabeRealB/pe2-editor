URL=https://download.videolan.org/videolan/x265/x265_4.2.tar.gz
PORT=x265; ARCHIVES="8bit/libx265.a=x265 10bit/libx265.a=x265_main10 12bit/libx265.a=x265_main12"
# Three encoders, as one: the 10- and 12-bit ones built without their C API, which the 8-bit one's dispatches to.
x265_cmake() { CMAKE_SOURCE=$SRC/source cmake_configure "$@" -DENABLE_SHARED=OFF -DENABLE_CLI=OFF -DENABLE_LIBNUMA=OFF -DENABLE_PIC=ON; }
configure() {
  mkdir -p 12bit 10bit 8bit
  (cd 12bit && SRC=$SRC x265_cmake -DHIGH_BIT_DEPTH=ON -DMAIN12=ON -DEXPORT_C_API=OFF) &&
  (cd 10bit && SRC=$SRC x265_cmake -DHIGH_BIT_DEPTH=ON -DEXPORT_C_API=OFF) &&
  (cd 8bit && SRC=$SRC x265_cmake -DEXTRA_LIB="x265_main10.a;x265_main12.a" -DEXTRA_LINK_FLAGS=-L. -DLINKED_10BIT=ON -DLINKED_12BIT=ON)
}
make_it() { ninja -C 12bit -v x265-static && ninja -C 10bit -v x265-static && ninja -C 8bit -v x265-static; }
install() {
  mkdir -p $SYS/include $SYS/lib/pkgconfig
  cp $SRC/source/x265.h 8bit/x265_config.h $SYS/include/
  cp 8bit/libx265.a $SYS/lib/libx265.a; cp 10bit/libx265.a $SYS/lib/libx265_main10.a; cp 12bit/libx265.a $SYS/lib/libx265_main12.a
  printf 'prefix=%s\nlibdir=${prefix}/lib\nincludedir=${prefix}/include\n\nName: x265\nDescription: H.265/HEVC video encoder\nVersion: 4.2\nLibs: -L${libdir} -lx265 -lx265_main10 -lx265_main12\nLibs.private: -lc++ %s\nCflags: -I${includedir}\n' $SYS "$( [ $HOST = linux ] && echo '-lpthread -lm -ldl' )" > $SYS/lib/pkgconfig/x265.pc
}
