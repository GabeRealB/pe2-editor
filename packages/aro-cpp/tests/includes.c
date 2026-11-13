/* Includes: a quoted one found beside the file, then in the `-iquote` folders, then in the `-I` ones; an angle one only
   in the `-I` ones; and a header included twice behind its guard. */
#include "beside.h"
#include "quoted.h"
#include <lib/public.h>
#include <lib/public.h>
int done = BESIDE + QUOTED + PUBLIC;
