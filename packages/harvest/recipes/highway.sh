URL=https://github.com/google/highway/releases/download/1.4.0/highway-1.4.0.tar.gz
PORT=highway; ARCHIVES=libhwy.a=hwy
configure() { cmake_configure -DHWY_ENABLE_TESTS=OFF -DHWY_ENABLE_EXAMPLES=OFF -DHWY_ENABLE_CONTRIB=OFF -DBUILD_TESTING=OFF -DHWY_FORCE_STATIC_LIBS=ON; }
make_it() { cmake_make; }
# It is C++, which its .pc file does not say for a static link.
install() { cmake_install && echo "Libs.private: -lc++" >> $SYS/lib/pkgconfig/libhwy.pc; }
