URL=https://github.com/webmproject/libwebp/archive/refs/tags/v1.6.0.tar.gz
PORT=webp; ARCHIVES="libwebp.a libsharpyuv.a libwebpmux.a"
configure() { cmake_configure -DWEBP_BUILD_ANIM_UTILS=OFF -DWEBP_BUILD_CWEBP=OFF -DWEBP_BUILD_DWEBP=OFF -DWEBP_BUILD_GIF2WEBP=OFF -DWEBP_BUILD_IMG2WEBP=OFF -DWEBP_BUILD_VWEBP=OFF -DWEBP_BUILD_WEBPINFO=OFF -DWEBP_BUILD_WEBPMUX=OFF -DWEBP_BUILD_EXTRAS=OFF -DWEBP_BUILD_LIBWEBPMUX=ON -DWEBP_USE_THREAD=ON; }
make_it() { cmake_make; }; install() { cmake_install; }
