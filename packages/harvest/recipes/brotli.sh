URL=https://github.com/google/brotli/archive/refs/tags/v1.2.1.tar.gz
PORT=brotli; ARCHIVES="libbrotlicommon.a libbrotlidec.a libbrotlienc.a"
configure() { cmake_configure -DBROTLI_BUILD_TOOLS=OFF -DBROTLI_DISABLE_TESTS=ON -DBUILD_TESTING=OFF; }
make_it() { cmake_make; }; install() { cmake_install; }
