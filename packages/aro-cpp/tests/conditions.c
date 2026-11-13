/* Conditions: arithmetic, `defined`, nesting, and what a definition given on the command line makes. */
#define VERSION 3
#if VERSION > 2 && (VERSION * 2) == 6 && !defined(UNDEFINED)
int arithmetic = 1;
#elif VERSION
int arithmetic = 2;
#else
int arithmetic = 3;
#endif
#ifdef VER_USA
#  if defined(USE_INCLUDE_ASM) || 0
int usa = 1;
#  endif
#else
int usa = 0;
#endif
#if (1 << 4) == 16 && -1 < 0 && 0x10 == 16 && 010 == 8 && 'A' == 65
int numbers = 1;
#endif
#undef VERSION
#ifndef VERSION
int undone = 1;
#endif
int line = __LINE__;
