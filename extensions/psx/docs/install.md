# Installing pe2.psx's tools

pe2.psx fetches what it builds with - its compilers, the binutils, maspsx and the Python that runs it - for Linux on
x86_64, macOS on Apple silicon and Windows: there is nothing to install. Elsewhere the binutils come from the system,
and a build looks for them first, saying which is missing.

## Linux

Everything is fetched for x86_64: there is nothing to install. The binutils and the preprocessor come from musl-cross's
toolchain for `mipsel`, which runs on glibc 2.34 or newer (Ubuntu 22.04, Debian 12, RHEL 9 and later).

On other machines, or to use your own, install GNU binutils built for `mips-linux-gnu` - on Debian and Ubuntu,
`sudo apt install binutils-mips-linux-gnu` - and they are used where nothing was fetched.

## macOS

Everything is fetched for Apple silicon: there is nothing to install. The binutils and the preprocessor come from
messense's macos-cross-toolchains for `mipsel`, which run on macOS 14 (Sonoma) or newer.

On Intel Macs nothing is fetched yet: pe2.psx does not build there.

## Windows

Everything is fetched: there is nothing to install.

The compiler fetched for Windows is SN Systems' build of GCC 2.8.1, as Sony's PlayStation Psy-Q SDK 4.4 distributed it,
from a copy of that SDK on GitHub. GCC is free software; the build is SN Systems', now Sony Interactive Entertainment's.
