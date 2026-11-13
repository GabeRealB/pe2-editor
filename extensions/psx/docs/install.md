# Installing pe2.psx's tools

pe2.psx fetches what it builds with - its compilers, the binutils, maspsx and the Python that runs it - for Linux on
x86_64, macOS on Apple silicon and Windows: there is nothing to install. Elsewhere the binutils come from the system,
and a build looks for them first, saying which is missing.

## Linux

Everything is fetched for x86_64: there is nothing to install. What is fetched needs nothing of the system's own
libraries, and runs on any Linux for x86_64.

On other machines, or to use your own, install GNU binutils built for `mips-linux-gnu` - on Debian and Ubuntu,
`sudo apt install binutils-mips-linux-gnu` - and they are used where nothing was fetched.

## macOS

Everything is fetched for Apple silicon: there is nothing to install. What is fetched runs on macOS 15 or newer.

On Intel Macs nothing is fetched yet: pe2.psx does not build there.

## Windows

Everything is fetched: there is nothing to install.

The compiler fetched for Windows is a 32-bit program, which every Windows that runs 32-bit programs runs.
