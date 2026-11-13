URL=https://github.com/sekrit-twc/zimg/archive/refs/tags/release-3.0.6.tar.gz
PORT=zimg; ARCHIVES=.libs/libzimg.a=zimg
# Its release is the repository's tree, without a configure script: autoreconf makes one.
configure() { (cd $SRC && PATH=$AUTOTOOLS_BIN:$PATH autoreconf -fi) && autotools_configure --disable-testapp --disable-example --disable-unit-test; }
make_it() { autotools_make; }; install() { autotools_install && sed -i 's/^\(Libs.private:.*\)$/\1 -lc++/' $SYS/lib/pkgconfig/zimg.pc; }
