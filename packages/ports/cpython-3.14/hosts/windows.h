/* pyconfig.h for Windows, with MinGW-w64 (MSYS2's CPython): what CPython 3.14.10's configure decided for that host, run with zig's C compiler for x86_64-windows-gnu
   (`--disable-shared --without-mimalloc --without-remote-debug --with-pkg-config=no`). */

#ifndef Py_PYCONFIG_H
#define Py_PYCONFIG_H
#define ALIGNOF_LONG 4
#define ALIGNOF_MAX_ALIGN_T 16
#define ALIGNOF_SIZE_T 8
#define DOUBLE_IS_LITTLE_ENDIAN_IEEE754 1
#define HAVE_ACCEPT 1
#define HAVE_ACOSH 1
#define HAVE_ADDRINFO 1
#define HAVE_ALIGNED_REQUIRED 1
#define HAVE_ASINH 1
#define HAVE_ATANH 1
#define HAVE_BIND 1
#define HAVE_BROKEN_UNSETENV 1
#define HAVE_BUILTIN_ATOMIC 1
#define HAVE_CHMOD 1
#define HAVE_CLOCK 1
#define HAVE_CLOCK_NANOSLEEP 1
#define HAVE_CLOCK_T 1
#define HAVE_CONIO_H 1
#define HAVE_CONNECT 1
#define HAVE_DECL_TZNAME 1
#define HAVE_DECL_UT_NAMESIZE 0
#define HAVE_DIRECT_H 1
#define HAVE_DIRENT_H 1
#define HAVE_DUP 1
#define HAVE_DUP2 1
#define HAVE_DYNAMIC_LOADING 1
#define HAVE_ERF 1
#define HAVE_ERFC 1
#define HAVE_ERRNO_H 1
#define HAVE_EXECV 1
#define HAVE_EXPM1 1
#define HAVE_FCNTL_H 1
#define HAVE_FSEEKO 1
#define HAVE_FTELLO 1
#define HAVE_FTIME 1
#define HAVE_GCC_ASM_FOR_X64 1
#define HAVE_GCC_ASM_FOR_X87 1
#define HAVE_GCC_UINT128_T 1
#define HAVE_GETHOSTBYADDR 1
#define HAVE_GETHOSTBYNAME 1
#define HAVE_GETHOSTNAME 1
#define HAVE_GETLOGIN 1
#define HAVE_GETPEERNAME 1
#define HAVE_GETPID 1
#define HAVE_GETPROTOBYNAME 1
#define HAVE_GETSERVBYNAME 1
#define HAVE_GETSERVBYPORT 1
#define HAVE_GETSOCKNAME 1
#define HAVE_INET_NTOA 1
#define HAVE_INET_PTON 1
#define HAVE_INTTYPES_H 1
#define HAVE_IO_H 1
#define HAVE_LARGEFILE_SUPPORT 1
#define HAVE_LISTEN 1
#define HAVE_LOG1P 1
#define HAVE_LOG2 1
#define HAVE_LONG_DOUBLE 1
#define HAVE_MBRTOWC 1
#define HAVE_MKTIME 1
#define HAVE_NANOSLEEP 1
#define HAVE_OPENDIR 1
#define HAVE_PROCESS_H 1
#define HAVE_PROTOTYPES 1
#define HAVE_PTHREAD_CONDATTR_SETCLOCK 1
#define HAVE_PTHREAD_COND_TIMEDWAIT_RELATIVE_NP 1
#define HAVE_PTHREAD_GETNAME_NP 1
#define HAVE_PTHREAD_SETNAME_NP 1
#define HAVE_RECVFROM 1
#define HAVE_SCHED_GET_PRIORITY_MAX 1
#define HAVE_SEM_GETVALUE 1
#define HAVE_SEM_UNLINK 1
#define HAVE_SENDTO 1
#define HAVE_SETJMP_H 1
#define HAVE_SETLOCALE 1
#define HAVE_SETSOCKOPT 1
#define HAVE_SETVBUF 1
#define HAVE_SHUTDOWN 1
#define HAVE_SIGNAL_H 1
#define HAVE_SNPRINTF 1
#define HAVE_SOCKADDR_STORAGE 1
#define HAVE_SOCKET 1
#define HAVE_SOCKLEN_T 1
#define HAVE_SSIZE_T 1
#define HAVE_STDINT_H 1
#define HAVE_STDIO_H 1
#define HAVE_STDLIB_H 1
#define HAVE_STD_ATOMIC 1
#define HAVE_STRFTIME 1
#define HAVE_STRINGS_H 1
#define HAVE_STRING_H 1
#define HAVE_STRUCT_STAT_ST_RDEV 1
#define HAVE_SYSTEM 1
#define HAVE_SYS_FILE_H 1
#define HAVE_SYS_PARAM_H 1
#define HAVE_SYS_STAT_H 1
#define HAVE_SYS_TIME_H 1
#define HAVE_SYS_TYPES_H 1
#define HAVE_TEMPNAM 1
#define HAVE_TMPFILE 1
#define HAVE_TMPNAM 1
#define HAVE_TZNAME 1
#define HAVE_UMASK 1
#define HAVE_UNISTD_H 1
#define HAVE_USABLE_WCHAR_T 1
#define HAVE_UTIME_H 1
#define HAVE_WCHAR_H 1
#define HAVE_WCSCOLL 1
#define HAVE_WCSFTIME 1
#define HAVE_WCSXFRM 1
#define HAVE_WMEMCMP 1
#define HAVE_WS2TCPIP_H 1
#define HAVE___UINT128_T 1
#define NT_THREADS 1
#define POSIX_SEMAPHORES_NOT_ENABLED 1
#define PY_BUILTIN_HASHLIB_HASHES "md5,sha1,sha2,sha3,blake2"
#define PY_SSL_DEFAULT_CIPHERS 1
#define PY_SUPPORT_TIER 0
#define RETSIGTYPE void
#define SIZEOF_DOUBLE 8
#define SIZEOF_FLOAT 4
#define SIZEOF_FPOS_T 8
#define SIZEOF_INT 4
#define SIZEOF_LONG 4
#define SIZEOF_LONG_DOUBLE 16
#define SIZEOF_LONG_LONG 8
#define SIZEOF_OFF_T 8
#define SIZEOF_PID_T 8
#define SIZEOF_SHORT 2
#define SIZEOF_SIZE_T 8
#define SIZEOF_TIME_T 8
#define SIZEOF_UINTPTR_T 8
#define SIZEOF_VOID_P 8
#define SIZEOF_WCHAR_T 2
#define SIZEOF__BOOL 1
#define STDC_HEADERS 1
#define SYS_SELECT_WITH_SYS_TIME 1
#ifndef _ALL_SOURCE
# define _ALL_SOURCE 1
#endif
#ifndef _DARWIN_C_SOURCE
# define _DARWIN_C_SOURCE 1
#endif
#ifndef __EXTENSIONS__
# define __EXTENSIONS__ 1
#endif
#ifndef _GNU_SOURCE
# define _GNU_SOURCE 1
#endif
#ifndef _HPUX_ALT_XOPEN_SOCKET_API
# define _HPUX_ALT_XOPEN_SOCKET_API 1
#endif
#ifndef _MINIX
#endif
#ifndef _NETBSD_SOURCE
# define _NETBSD_SOURCE 1
#endif
#ifndef _OPENBSD_SOURCE
# define _OPENBSD_SOURCE 1
#endif
#ifndef _POSIX_SOURCE
#endif
#ifndef _POSIX_1_SOURCE
#endif
#ifndef _POSIX_PTHREAD_SEMANTICS
# define _POSIX_PTHREAD_SEMANTICS 1
#endif
#ifndef __STDC_WANT_IEC_60559_ATTRIBS_EXT__
# define __STDC_WANT_IEC_60559_ATTRIBS_EXT__ 1
#endif
#ifndef __STDC_WANT_IEC_60559_BFP_EXT__
# define __STDC_WANT_IEC_60559_BFP_EXT__ 1
#endif
#ifndef __STDC_WANT_IEC_60559_DFP_EXT__
# define __STDC_WANT_IEC_60559_DFP_EXT__ 1
#endif
#ifndef __STDC_WANT_IEC_60559_EXT__
# define __STDC_WANT_IEC_60559_EXT__ 1
#endif
#ifndef __STDC_WANT_IEC_60559_FUNCS_EXT__
# define __STDC_WANT_IEC_60559_FUNCS_EXT__ 1
#endif
#ifndef __STDC_WANT_IEC_60559_TYPES_EXT__
# define __STDC_WANT_IEC_60559_TYPES_EXT__ 1
#endif
#ifndef __STDC_WANT_LIB_EXT2__
# define __STDC_WANT_LIB_EXT2__ 1
#endif
#ifndef __STDC_WANT_MATH_SPEC_FUNCS__
# define __STDC_WANT_MATH_SPEC_FUNCS__ 1
#endif
#ifndef _TANDEM_SOURCE
# define _TANDEM_SOURCE 1
#endif
#ifndef _XOPEN_SOURCE
# define _XOPEN_SOURCE 700
#endif
#define WITH_DECIMAL_CONTEXTVAR 1
#define WITH_DOC_STRINGS 1
#define WITH_PYMALLOC 1
#if defined AC_APPLE_UNIVERSAL_BUILD
# if defined __BIG_ENDIAN__
#  define WORDS_BIGENDIAN 1
# endif
#else
# ifndef WORDS_BIGENDIAN
# endif
#endif
#define _DARWIN_C_SOURCE 1
#define _FILE_OFFSET_BITS 64
#define _LARGEFILE_SOURCE 1
#define _NETBSD_SOURCE 1
#define _POSIX_C_SOURCE 200809L
#define _PYTHONFRAMEWORK ""
#define _PYTHREAD_NAME_MAXLEN 32766
#define _Py_HACL_CAN_COMPILE_VEC128 1
#define _Py_HACL_CAN_COMPILE_VEC256 1
#define _Py_STACK_GROWS_DOWN 1
#define _XOPEN_SOURCE 700
#define _XOPEN_SOURCE_EXTENDED 1
#define __BSD_VISIBLE 1
#define gid_t int
#define uid_t int
#if defined(__USLC__) && defined(__SCO_VERSION__)
#define STRICT_SYSV_CURSES
#endif
#endif
