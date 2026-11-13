/* opcodes/config.h for Windows, with MinGW-w64: what binutils 2.47's configure decided for that host (`--target=mipsel-linux-gnu`,
   without NLS, plugins, zstd or libctf), run with zig's C compiler for x86_64-windows-gnu. */

#if defined(__GLIBC__) && !defined(__FreeBSD_kernel__) && !defined(__CONFIG_H__)
#  error config.h must be #included before system headers
#endif
#define __CONFIG_H__ 1

#define ENABLE_CHECKING 1

#define HAVE_DECL_BASENAME 0

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

#define LT_OBJDIR ".libs/"

#define PACKAGE "opcodes"

#define PACKAGE_BUGREPORT ""

#define PACKAGE_NAME "opcodes"

#define PACKAGE_STRING "opcodes 2.47"

#define PACKAGE_TARNAME "opcodes"

#define PACKAGE_URL ""

#define PACKAGE_VERSION "2.47"

#define STDC_HEADERS 1

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
