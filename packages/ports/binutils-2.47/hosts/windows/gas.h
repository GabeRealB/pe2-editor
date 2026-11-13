/* gas/config.h for Windows, with MinGW-w64: what binutils 2.47's configure decided for that host (`--target=mipsel-linux-gnu`,
   without NLS, plugins, zstd or libctf), run with zig's C compiler for x86_64-windows-gnu. */

#if defined(__GLIBC__) && !defined(__FreeBSD_kernel__) && !defined(__CONFIG_H__)
#  error config.h must be #included before system headers
#endif
#define __CONFIG_H__ 1

#define CROSS_COMPILE 1

#define DEFAULT_COMPRESSED_DEBUG_ALGORITHM COMPRESS_DEBUG_GABI_ZLIB

#define DEFAULT_EMULATION "mipslelf"

#define DEFAULT_GENERATE_BUILD_NOTES 0

#define DEFAULT_GENERATE_ELF_STT_COMMON 0

#define DEFAULT_MIPS_FIX_LOONGSON3_LLSC 0

#define DEFAULT_RISCV_ATTR 1

#define DEFAULT_SFRAME 0

#define DEFAULT_X86_TLS_CHECK 1

#define DEFAULT_X86_USED_NOTE 0

#define EMULATIONS &mipslelf, &mipsbelf, &mipself,

#define ENABLE_CHECKING 1

#define ENABLE_LEAK_CHECK BFD_ASAN

#define HAVE_DECL_GETOPT 1

#define HAVE_DECL_MEMPCPY 1

#define HAVE_DECL_STPCPY 0

#define HAVE_INTTYPES_H 1

#define HAVE_MEMORY_H 1

#define HAVE_STDINT_H 1

#define HAVE_STDLIB_H 1

#define HAVE_STRINGS_H 1

#define HAVE_STRING_H 1

#define HAVE_SYS_STAT_H 1

#define HAVE_SYS_TYPES_H 1

#define HAVE_UNISTD_H 1

#define HAVE_WINDOWS_H 1

#define LT_OBJDIR ".libs/"

#define MIPS_CPU_STRING_DEFAULT "from-abi"

#define MIPS_DEFAULT_64BIT 0

#define MIPS_DEFAULT_ABI O32_ABI

#define NEED_DECLARATION_FFS 1

#define PACKAGE "gas"

#define PACKAGE_BUGREPORT ""

#define PACKAGE_NAME "gas"

#define PACKAGE_STRING "gas 2.47"

#define PACKAGE_TARNAME "gas"

#define PACKAGE_URL ""

#define PACKAGE_VERSION "2.47"

#define STDC_HEADERS 1

#define TARGET_ALIAS "mipsel-linux-gnu"

#define TARGET_BYTES_BIG_ENDIAN 0

#define TARGET_CANONICAL "mipsel-unknown-linux-gnu"

#define TARGET_CPU "mipsel"

#define TARGET_OS "linux-gnu"

#define TARGET_VENDOR "unknown"

#define USE_BINARY_FOPEN 1

#define USE_EF_MIPS_ABI_O32 1

#define USE_EMULATIONS 1

#ifndef _ALL_SOURCE
# define _ALL_SOURCE 1
#endif

#ifndef _GNU_SOURCE
# define _GNU_SOURCE 1
#endif

#ifndef _POSIX_PTHREAD_SEMANTICS
# define _POSIX_PTHREAD_SEMANTICS 1
#endif

#ifndef _TANDEM_SOURCE
# define _TANDEM_SOURCE 1
#endif

#ifndef __EXTENSIONS__
# define __EXTENSIONS__ 1
#endif

#define VERSION "2.47"

#if defined AC_APPLE_UNIVERSAL_BUILD
# if defined __BIG_ENDIAN__
#  define WORDS_BIGENDIAN 1
# endif
#else
# ifndef WORDS_BIGENDIAN

# endif
#endif

#ifndef _DARWIN_USE_64_BIT_INODE
# define _DARWIN_USE_64_BIT_INODE 1
#endif

#define _FILE_OFFSET_BITS 64
