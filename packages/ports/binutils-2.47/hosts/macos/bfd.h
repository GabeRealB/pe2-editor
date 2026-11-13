/* bfd/config.h for macOS: what binutils 2.47's configure decided for that host (`--target=mipsel-linux-gnu`,
   without NLS, plugins, zstd or libctf), run with zig's C compiler for aarch64-macos. */

#if defined(__GLIBC__) && !defined(__FreeBSD_kernel__) && !defined(__CONFIG_H__)
#  error config.h must be #included before system headers
#endif
#define __CONFIG_H__ 1

#define DEFAULT_LD_Z_SEPARATE_CODE 0

#define ENABLE_CHECKING 1

#define HAVE_DECL_ASPRINTF 1

#define HAVE_DECL_BASENAME 0

#define HAVE_DECL_FFS 1

#define HAVE_DECL_FOPEN64 0

#define HAVE_DECL_FSEEKO 1

#define HAVE_DECL_FSEEKO64 0

#define HAVE_DECL_FTELLO 1

#define HAVE_DECL_FTELLO64 0

#define HAVE_DECL_STPCPY 1

#define HAVE_DECL_STRNLEN 1

#define HAVE_DECL_STRTOULL 1

#define HAVE_DECL_VASPRINTF 1

#define HAVE_DECL____LC_CODEPAGE_FUNC 0

#define HAVE_DLFCN_H 1

#define HAVE_FCNTL 1

#define HAVE_FCNTL_H 1

#define HAVE_FDOPEN 1

#define HAVE_FILENO 1

#define HAVE_FLS 1

#define HAVE_FSEEKO 1

#define HAVE_FTELLO 1

#define HAVE_GETGID 1

#define HAVE_GETPAGESIZE 1

#define HAVE_GETRLIMIT 1

#define HAVE_GETUID 1

#define HAVE_HIDDEN 1

#define HAVE_INTTYPES_H 1

#define HAVE_MADVISE 1

#define HAVE_MEMORY_H 1

#define HAVE_MPROTECT 1

#define HAVE_STDINT_H 1

#define HAVE_STDLIB_H 1

#define HAVE_STRINGS_H 1

#define HAVE_STRING_H 1

#define HAVE_SYSCONF 1

#define HAVE_SYS_FILE_H 1

#define HAVE_SYS_PARAM_H 1

#define HAVE_SYS_RESOURCE_H 1

#define HAVE_SYS_STAT_H 1

#define HAVE_SYS_TYPES_H 1

#define HAVE_UNISTD_H 1

#define LT_OBJDIR ".libs/"

#define PACKAGE "bfd"

#define PACKAGE_BUGREPORT ""

#define PACKAGE_NAME "bfd"

#define PACKAGE_STRING "bfd 2.47"

#define PACKAGE_TARNAME "bfd"

#define PACKAGE_URL ""

#define PACKAGE_VERSION "2.47"

#define SIZEOF_INT 4

#define SIZEOF_LONG 8

#define SIZEOF_LONG_LONG 8

#define SIZEOF_OFF_T 8

#define SIZEOF_VOID_P 8

#define STDC_HEADERS 1

#define TLS _Thread_local

#define USE_SECUREPLT 1

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
