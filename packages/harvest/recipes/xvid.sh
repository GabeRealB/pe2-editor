# The release's zip, holding what its tarball does: the tarball's server labels it a plain tar, which zig believes.
URL=https://downloads.xvid.com/downloads/xvidcore-1.3.7.zip
PORT=xvid; ARCHIVES='$SRC/build/generic/=build/$(xvid_lib)=xvidcore'
# Its static library's name: Windows' has no lib.
xvid_lib() { [ $HOST = windows ] && echo xvidcore.a || echo libxvidcore.a; }
# Configured and made in its source folder (build/generic), as its build is; x86's assembly by nasm.
configure() { cd $SRC/build/generic && ./configure --host=$TRIPLE --prefix=$SYS $( [ $HOST = macos ] && echo --disable-assembly ); }
make_it() { make -C $SRC/build/generic -j$JOBS $(xvid_lib); }
install() { mkdir -p $SYS/include; cp $SRC/src/xvid.h $SYS/include/; cp "$SRC/build/generic/=build/$(xvid_lib)" $SYS/lib/libxvidcore.a; }
