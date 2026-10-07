/* Exercises every kind of declaration wid_cimport imports. */
#ifndef KITCHEN_H
#define KITCHEN_H

#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

#include "detail/part.h"
#include "../outside.h"

/** Adds two numbers. */
int add(int a, int b);
/// Logs a formatted message.
void log_message(const char *restrict format, ...);
static inline int twice(int x) { return x * 2; }
size_t write_bytes(FILE *file, const uint8_t *bytes, size_t len); // Writes bytes.
void takes_array(int values[4], void callback(int code));
char *const *argv_like(char *const *argv);
void unnamed(int, float);
int no_params(void);
Outside *get_outside(outside_id id);
Part make_part(int id);
int add(int a, int b);

typedef struct Vec2 {
    float x; // Horizontal.
    float y; ///< Vertical.
} Vec2;

typedef struct {
    int w, h;
} Size;

typedef struct Handle Handle;

struct Later;

typedef struct tagLegacy {
    int v;
} Legacy;

struct Later {
    struct Later *next;
    int value;
};

struct Flags {
    unsigned int visible : 1;
    unsigned int : 0;
    unsigned int layer : 4;
};

struct Packet {
    uint32_t len;
    uint8_t data[];
};

struct Shape {
    enum ShapeKind { SHAPE_CIRCLE, SHAPE_RECT } kind;
    struct Point {
        int x, y;
    } origin;
    union {
        float radius;
        Size size;
    };
    struct {
        int r, g, b;
    } color;
};

union Number {
    int i;
    float f;
    double d;
};

enum Color8 : uint8_t { COLOR_RED = 1, COLOR_GREEN = 2, COLOR_MAX = 255 };
enum Signed { NEG = -5, ZERO = 0 };
enum Big : unsigned long long { BIG = 0xFFFFFFFFFFFFFFFFull };
enum { ANON_A = 10, ANON_B };
typedef enum { MODE_ON, MODE_OFF } Mode;

typedef void (*Callback)(int code, void *user_data);
typedef void Handler(int signal);
typedef Vec2 Point2;
typedef const char *CString;
typedef int Matrix[4][4];

extern int global_counter;
extern const char *const version_string;
extern _Atomic int atomic_counter;
extern volatile int hardware_register;
extern thread_local int per_thread;
static const int static_limit = 7;
extern Callback global_hook;
extern int (*raw_hook)(int value);
extern va_list *va_pointer;

#define KITCHEN_INT 42
#define KITCHEN_NEG (-7)
#define KITCHEN_HEX 0xFFu
#define KITCHEN_SHIFT 1ull << 40
#define KITCHEN_FLOAT 1.5f
#define KITCHEN_DOUBLE 2.25
#define KITCHEN_STR "kitchen"
#define KITCHEN_PSTR ("paren" "\x41")
#define KITCHEN_CHAR 'k'
#define KITCHEN_BOOL true
#define KITCHEN_ENUM COLOR_GREEN
#define KITCHEN_VEC (Vec2){ 1.0f, 2.0f }
#define KITCHEN_CAST ((uint16_t)300)
#define KITCHEN_NULL ((void *)0)
#define KITCHEN_EMPTY
#define KITCHEN_API extern
#define KITCHEN_TYPE int
#define KITCHEN_COMMA 1, 2
#define KITCHEN_UNBALANCED (
#define KITCHEN_UNDECLARED not_declared_anywhere
#define KITCHEN_MAX(a, b) ((a) > (b) ? (a) : (b))
#define KITCHEN_LOG(fmt, ...) log_message(fmt, __VA_ARGS__)
#define KITCHEN_ALIAS add
/** The answer. */
#define KITCHEN_ANSWER 42
#define KITCHEN_TRAILING 3 // Three.

#endif
