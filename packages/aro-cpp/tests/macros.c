/* Macros as the game's sources write them: variadic ones counting their arguments, stringizing, pasting, and
   expansion through several levels. */
#define COUNT(...) COUNT_(__VA_ARGS__, 6, 5, 4, 3, 2, 1, 0)
#define COUNT_(a, b, c, d, e, f, n, ...) n
#define PICK(...) PICK_(COUNT(__VA_ARGS__), __VA_ARGS__)
#define PICK_(n, ...) PICK__(n, __VA_ARGS__)
#define PICK__(n, ...) PICK_##n(__VA_ARGS__)
#define PICK_1(a) one(a)
#define PICK_2(a, b) two(a, b)
#define PICK_3(a, b, c) three(a, b, c)
#define STR(x) #x
#define XSTR(x) STR(x)
#define CAT(a, b) a##b
#define XCAT(a, b) CAT(a, b)
#define SIZE 0x24
#define FIELD(base, n) (*(volatile unsigned int *)((base) + (n) * SIZE))
#define EMPTY
#define LATER(x) x EMPTY
#define SELF SELF + 1
#define CALL(f, ...) f(__VA_ARGS__)

int a = PICK(1);
int b = PICK(1, 2);
int c = PICK(1, 2, 3);
const char *s = STR(a + b) XSTR(SIZE) STR("quoted \"text\"") STR( spaced   out );
int XCAT(name_, SIZE) = CAT(0x, 10);
unsigned int f = FIELD(0x1F801000, 3);
int g = LATER(4) + SELF;
int h = CALL(max, 1, (2, 3), [4]);
