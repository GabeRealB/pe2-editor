/* ld/config.h for Windows, with MinGW-w64: what binutils 2.47's configure decided for that host (`--target=mipsel-linux-gnu`,
   without NLS, plugins, zstd or libctf), run with zig's C compiler for x86_64-windows-gnu. */

#if defined(__GLIBC__) && !defined(__FreeBSD_kernel__) && !defined(__CONFIG_H__)
#  error config.h must be #included before system headers
#endif
#define __CONFIG_H__ 1

#define DEFAULT_COMPRESSED_DEBUG_ALGORITHM COMPRESS_DEBUG_GABI_ZLIB

#define DEFAULT_EMIT_GNU_HASH 0

#define DEFAULT_EMIT_SYSV_HASH 1

#define DEFAULT_LD_ERROR_EXECSTACK 0

#define DEFAULT_LD_ERROR_RWX_SEGMENTS 0

#define DEFAULT_LD_EXECSTACK 1

#define DEFAULT_LD_GNU2_TLS_TAG 2

#define DEFAULT_LD_GNU_TLS_TAG 2

#define DEFAULT_LD_ROSEGMENT 0

#define DEFAULT_LD_TEXTREL_CHECK textrel_check_none

#define DEFAULT_LD_TEXTREL_CHECK_WARNING 0

#define DEFAULT_LD_WARN_EXECSTACK 0

#define DEFAULT_LD_WARN_RWX_SEGMENTS 0

#define DEFAULT_LD_Z_MARK_PLT 0

#define DEFAULT_LD_Z_MEMORY_SEAL 0

#define DEFAULT_LD_Z_RELRO 0

#define DEFAULT_LD_Z_SEPARATE_CODE 0

#define DEFAULT_NEW_DTAGS 0

#define ENABLE_CHECKING 1

#define GOT_HANDLING_DEFAULT GOT_HANDLING_TARGET_DEFAULT

#define HAVE_CLOSE 1

#define HAVE_DECL_ENVIRON 1

#define HAVE_DECL_GETOPT 1

#define HAVE_DECL_STPCPY 0

#define HAVE_DECL_STRTOULL 1

#define HAVE_FCNTL_H 1

#define HAVE_INITFINI_ARRAY 1

#define HAVE_INTTYPES_H 1

#define HAVE_LIMITS_H 1

#define HAVE_LSEEK 1

#define HAVE_MEMORY_H 1

#define HAVE_MKSTEMP 1

#define HAVE_OPEN 1

#define HAVE_STDINT_H 1

#define HAVE_STDLIB_H 1

#define HAVE_STRINGS_H 1

#define HAVE_STRING_H 1

#define HAVE_SYS_FILE_H 1

#define HAVE_SYS_PARAM_H 1

#define HAVE_SYS_STAT_H 1

#define HAVE_SYS_TIME_H 1

#define HAVE_SYS_TYPES_H 1

#define HAVE_UNISTD_H 1

#define HAVE_WINDOWS_H 1

#define LT_OBJDIR ".libs/"

#define PACKAGE "ld"

#define PACKAGE_BUGREPORT ""

#define PACKAGE_NAME "ld"

#define PACKAGE_STRING "ld 2.47"

#define PACKAGE_TARNAME "ld"

#define PACKAGE_URL ""

#define PACKAGE_VERSION "2.47"

#define STDC_HEADERS 1

#define SUPPORT_ERROR_HANDLING_SCRIPT 1

#define USE_BINARY_FOPEN 1

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

#ifndef _DARWIN_USE_64_BIT_INODE
# define _DARWIN_USE_64_BIT_INODE 1
#endif

#define _FILE_OFFSET_BITS 64
