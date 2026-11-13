# Debian's copy of the release (byte for byte the one at quut.com, whose server zig's HTTP client cannot read).
URL=https://deb.debian.org/debian/pool/main/libg/libgsm/libgsm_1.0.24.orig.tar.gz
PORT=gsm; ARCHIVES='$SRC/lib/libgsm.a=gsm'
# Made in its source folder, by its Makefile alone, with the flags it is configured by there.
# Its rule compiles where it runs and moves the object, whose name zig gives Windows' (.obj) the rule does not
# expect: the rule names its output.
configure() { sed -i 's/^\t\t$(CC) $(CFLAGS) $?$/\t\t$(CC) $(CFLAGS) -o $@ $?/' $SRC/Makefile; }
make_it() { make -C $SRC -j$JOBS lib/libgsm.a CC="$CC -ansi -pedantic" AR=$AR RANLIB=$RANLIB; }
install() { mkdir -p $SYS/include/gsm; cp $SRC/inc/gsm.h $SYS/include/gsm/; cp $SRC/lib/libgsm.a $SYS/lib/; }
