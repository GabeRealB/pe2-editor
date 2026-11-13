URL=https://github.com/libjxl/libjxl/archive/refs/tags/v0.13.0.tar.gz
PORT=jxl; ARCHIVES="lib/libjxl.a=jxl lib/libjxl_threads.a=jxl_threads lib/libjxl_cms.a=jxl_cms"
configure() { cmake_configure -DJPEGXL_FORCE_SYSTEM_HWY=ON -DJPEGXL_FORCE_SYSTEM_BROTLI=ON -DJPEGXL_FORCE_SYSTEM_LCMS2=ON -DJPEGXL_ENABLE_SKCMS=OFF -DJPEGXL_ENABLE_TOOLS=OFF -DJPEGXL_ENABLE_EXAMPLES=OFF -DJPEGXL_ENABLE_JNI=OFF -DJPEGXL_ENABLE_MANPAGES=OFF -DJPEGXL_ENABLE_BENCHMARK=OFF -DJPEGXL_ENABLE_DOXYGEN=OFF -DJPEGXL_ENABLE_SJPEG=OFF -DJPEGXL_ENABLE_OPENEXR=OFF -DJPEGXL_ENABLE_JPEGLI=OFF -DJPEGXL_ENABLE_PLUGINS=OFF -DJPEGXL_ENABLE_VIEWERS=OFF -DJPEGXL_ENABLE_TRANSCODE_JPEG=ON -DJPEGXL_BUNDLE_LIBPNG=OFF -DJPEGXL_STATIC=OFF -DBUILD_TESTING=OFF; }
make_it() { cmake_make; }
# They are C++, which their .pc files do not say for a static link.
install() { cmake_install && for pc in libjxl libjxl_threads libjxl_cms; do sed -i 's/^\(Libs.private:.*\)$/\1 -lc++/' $SYS/lib/pkgconfig/$pc.pc; done; }
