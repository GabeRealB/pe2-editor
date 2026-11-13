URL=https://github.com/AOMediaCodec/SVT-AV1/archive/refs/tags/v4.3.0.tar.gz
PORT=svt-av1; ARCHIVES='$(svt_lib)=SvtAv1Enc'
# Its archive is made in the source tree's Bin folder.
svt_lib() { echo $SRC/Bin/Release/libSvtAv1Enc.a; }
configure() { cmake_configure -DBUILD_APPS=OFF -DBUILD_TESTING=OFF -DBUILD_DEC=OFF -DSVT_AV1_LTO=OFF -DENABLE_AVX512=ON; }
make_it() { cmake_make; }; install() { cmake_install; }
