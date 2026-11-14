# gcc-psx

What each release of the package changed, newest first: a release's entry is its notes' "What changed".

## 2.8.1-2

A store to a constant address is never taken for one instruction, as GCC 2.8.1 never took it on the hosts it was built
for. `simple_memory_operand` tested the pointer to the address rather than the address; built for a 64-bit host, cc1 read
that pointer's low 32 bits, which an allocator placing memory at random leaves within range about once in 65,536, so
about one compile in a few thousand put such a store (the scratchpad's, `0x1F8003FC`) in a branch's delay slot: the same
source compiled otherwise from one run to the next (`ports/gcc-2.8.1/patches/simple-memory-const-address.patch`). The
Windows build, 32-bit, compiles as it did.

## 2.8.1-1

First release: GCC 2.8.1's C compiler (cc1) for the PlayStation, with old-gcc's patches and the port's for 64-bit hosts.
