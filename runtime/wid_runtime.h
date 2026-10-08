/*
 * wid_runtime.h: the runtime support library for programs compiled by Wid.
 *
 * Generated programs are a single C23 translation unit that defines
 * WID_RUNTIME_IMPLEMENTATION before including this header. Everything here is
 * prefixed with `wid_` (functions, types) or `WID_` (macros). Programs are
 * compiled with -fwrapv and -fno-strict-aliasing: typed container headers are
 * accessed through the type-erased views declared here.
 */
#ifndef WID_RUNTIME_H
#define WID_RUNTIME_H

#include <errno.h>
#include <math.h>
#include <stdarg.h>
#include <stdckdint.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

static_assert(sizeof(void *) == 8, "Wid currently targets 64-bit platforms");
static_assert(sizeof(double) == 8 && sizeof(float) == 4, "IEEE 754 floats are required");

typedef int64_t wid_Int;
typedef uint64_t wid_UInt;
typedef int32_t wid_Rune;
typedef uint32_t wid_Error;
typedef uint64_t wid_TypeId;

/** An immutable UTF-8 string view: a pointer and a length in bytes. */
typedef struct wid_String {
    const uint8_t *data;
    wid_Int len;
} wid_String;

/** Builds a wid_String from a C string literal. */
/* The assembler name of the C symbol `name`. Imported C functions are declared
   under a `wid_extern_` name bound to their symbol, so the declaration can't
   conflict with a header's declaration of the same function. */
#define WID_STRINGIFY_(x) #x
#define WID_STRINGIFY(x) WID_STRINGIFY_(x)
#define WID_SYMBOL(name) WID_STRINGIFY(__USER_LABEL_PREFIX__) name

#define WID_STR(lit) ((wid_String){(const uint8_t *)(lit), (wid_Int)(sizeof(lit) - 1)})

/** A source position, used by panics and the tracking allocator. */
typedef struct wid_Location {
    char *file;
    int32_t line;
    int32_t column;
    char *proc;
} wid_Location;

#define WID_LOC(file, line, column, proc) ((wid_Location){(file), (line), (column), (proc)})

/** What an allocator procedure is asked to do. */
typedef enum wid_AllocMode : uint8_t {
    WID_ALLOC = 0,
    WID_FREE = 1,
    WID_FREE_ALL = 2,
    WID_RESIZE = 3,
} wid_AllocMode;

/** The allocator procedure. Returns the new block, or nullptr. */
typedef void *(*wid_AllocProc)(void *data, wid_AllocMode mode, wid_Int size, wid_Int align, void *old,
                                wid_Int old_size, wid_Location loc);

/** An allocator: a procedure and its state. */
typedef struct wid_Allocator {
    wid_AllocProc proc;
    void *data;
} wid_Allocator;

/** A logger: a procedure and its state. */
typedef struct wid_Logger {
    void (*proc)(void *data, int32_t level, wid_String message, wid_Location loc);
    void *data;
} wid_Logger;

/** The implicit context passed to every Wid procedure. */
typedef struct wid_Context {
    wid_Allocator allocator;
    wid_Allocator temp_allocator;
    wid_Logger logger;
    void *user_data;
    wid_Int user_index;
} wid_Context;

static_assert(sizeof(wid_Context) == 64, "the compiler assumes a 64-byte context");
static_assert(sizeof(wid_Allocator) == 16, "the compiler assumes a 16-byte allocator");

/** A type-erased slice: `[]T`. */
typedef struct wid_RawSlice {
    void *data;
    wid_Int len;
} wid_RawSlice;

/** A type-erased dynamic array: `[dynamic]T`. */
typedef struct wid_RawDyn {
    void *data;
    wid_Int len;
    wid_Int cap;
    wid_Allocator allocator;
} wid_RawDyn;

/** A type-erased hash map: `map[K]V`. */
typedef struct wid_RawMap {
    uint8_t *slots;
    wid_Int len;
    wid_Int cap;
    wid_Allocator allocator;
} wid_RawMap;

static_assert(sizeof(wid_RawDyn) == 40 && sizeof(wid_RawMap) == 40, "the compiler assumes 40-byte containers");

/**
 * Describes the slot layout of one map type. Keys are equal when `==` says
 * so: a key type whose bytes don't decide that (one holding a float, where
 * `-0.0` is `0.0`, a string inside a struct, an optional or padding) has a
 * `key_hash` and a `key_eq` the compiler generates; the others are hashed
 * and compared by their bytes, and a `String` by its text.
 */
typedef struct wid_MapInfo {
    wid_Int key_size;
    wid_Int value_size;
    wid_Int slot_size;
    wid_Int key_offset;
    wid_Int value_offset;
    wid_Int align;
    bool string_key;
    uint64_t (*key_hash)(const void *key);
    bool (*key_eq)(const void *a, const void *b);
} wid_MapInfo;

/** Feeds `len` bytes into a map key's FNV-1a hash `h`. */
static inline uint64_t wid_hash_feed(uint64_t h, const void *data, wid_Int len) {
    const uint8_t *p = data;
    for (wid_Int i = 0; i < len; i++) {
        h ^= p[i];
        h *= 0x100000001b3ull;
    }
    return h;
}
/** Feeds a float into a map key's hash, with `-0.0` hashed as `0.0`. */
static inline uint64_t wid_hash_f64(uint64_t h, double v) {
    if (v == 0) v = 0;
    return wid_hash_feed(h, &v, sizeof v);
}
/** Feeds an `F32` into a map key's hash, with `-0.0` hashed as `0.0`. */
static inline uint64_t wid_hash_f32(uint64_t h, float v) {
    if (v == 0) v = 0;
    return wid_hash_feed(h, &v, sizeof v);
}

/** A text sink: a C stream, or a growable buffer owned by an allocator. */
typedef struct wid_Writer {
    FILE *file;
    uint8_t *buf;
    wid_Int len;
    wid_Int cap;
    wid_Allocator allocator;
} wid_Writer;

static_assert(sizeof(wid_Writer) == 48, "the compiler assumes a 48-byte writer");

/* ----- process ------------------------------------------------------------ */

/** Records the command line and prepares stdout buffering. */
void wid_runtime_init(int argc, char **argv);
/** Flushes output; called when `main` returns. */
void wid_runtime_exit(void);
/** Returns the default context: heap allocator, temp arena, stderr logger. */
wid_Context wid_default_context(void);
/** Returns the command-line arguments as strings, program name first. */
wid_String wid_arg(wid_Int i);
/** Returns the number of command-line arguments. */
wid_Int wid_arg_count(void);

/* ----- panics ------------------------------------------------------------- */

/** Prints `panic: message` with the location and exits with status 101. */
[[noreturn]] void wid_panic(wid_String message, wid_Location loc);
/** Prints a formatted panic message with the location and exits. */
[[noreturn]] void wid_panicf(wid_Location loc, const char *fmt, ...);

/** Panics with `message` when `cond` is false. */
static inline void wid_assert(bool cond, wid_String message, wid_Location loc) {
    if (!cond) wid_panic(message, loc);
}

/** Returns `i` when it is a valid index below `len`, otherwise panics. */
static inline wid_Int wid_bounds(wid_Int i, wid_Int len, wid_Location loc) {
    if ((uint64_t)i >= (uint64_t)len) {
        wid_panicf(loc, "index %lld is out of bounds for length %lld", (long long)i, (long long)len);
    }
    return i;
}

/** Checks that `lo..hi` (exclusive) lies within `0..len`. */
static inline void wid_slice_check(wid_Int lo, wid_Int hi, wid_Int len, wid_Location loc) {
    if (lo < 0 || hi < lo || hi > len) {
        wid_panicf(loc, "range %lld...%lld is out of bounds for length %lld", (long long)lo, (long long)hi,
                   (long long)len);
    }
}

/** Panics when an optional without a value is unwrapped. */
[[noreturn]] void wid_nil_panic(wid_Location loc);

/* ----- integer arithmetic -------------------------------------------------- */

/*
 * Checked operations trap on overflow (debug builds). Unchecked arithmetic is
 * compiled with -fwrapv, so it wraps. Division always checks for zero.
 */
#define WID_INT_OPS(T, S, MIN)                                                                       \
    static inline T wid_add_##S(T a, T b, wid_Location loc) {                                         \
        T r;                                                                                         \
        if (ckd_add(&r, a, b)) wid_panicf(loc, "integer overflow in `+`");                           \
        return r;                                                                                    \
    }                                                                                                \
    static inline T wid_sub_##S(T a, T b, wid_Location loc) {                                         \
        T r;                                                                                         \
        if (ckd_sub(&r, a, b)) wid_panicf(loc, "integer overflow in `-`");                           \
        return r;                                                                                    \
    }                                                                                                \
    static inline T wid_mul_##S(T a, T b, wid_Location loc) {                                         \
        T r;                                                                                         \
        if (ckd_mul(&r, a, b)) wid_panicf(loc, "integer overflow in `*`");                           \
        return r;                                                                                    \
    }                                                                                                \
    static inline T wid_div_##S(T a, T b, wid_Location loc) {                                         \
        if (b == 0) wid_panicf(loc, "division by zero");                                             \
        if (MIN != 0 && a == (T)MIN && b == (T)-1) return a;                                         \
        return (T)(a / b);                                                                           \
    }                                                                                                \
    static inline T wid_rem_##S(T a, T b, wid_Location loc) {                                         \
        if (b == 0) wid_panicf(loc, "division by zero");                                             \
        if (MIN != 0 && a == (T)MIN && b == (T)-1) return 0;                                         \
        return (T)(a % b);                                                                           \
    }                                                                                                \
    static inline T wid_pow_##S(T base, T exp, wid_Location loc) {                                    \
        if (wid_negative_##S(exp)) wid_panicf(loc, "negative exponent in integer `**`");           \
        T result = 1;                                                                                \
        while (exp > 0) {                                                                            \
            if (exp & 1) result = (T)(result * base);                                                \
            base = (T)(base * base);                                                                 \
            exp = (T)(exp >> 1);                                                                     \
        }                                                                                            \
        return result;                                                                               \
    }                                                                                                \
    /* `**` in debug builds. The base is squared only while bits of the */                           \
    /* exponent remain, so a square overflows only when the result does. */                          \
    static inline T wid_pow_checked_##S(T base, T exp, wid_Location loc) {                            \
        if (wid_negative_##S(exp)) wid_panicf(loc, "negative exponent in integer `**`");           \
        T result = 1;                                                                                \
        while (exp > 0) {                                                                            \
            if ((exp & 1) && ckd_mul(&result, result, base)) goto overflow;                          \
            exp = (T)(exp >> 1);                                                                     \
            if (exp > 0 && ckd_mul(&base, base, base)) goto overflow;                                \
        }                                                                                            \
        return result;                                                                               \
    overflow:                                                                                        \
        wid_panicf(loc, "integer overflow in `**`");                                                 \
    }

static inline bool wid_negative_i8(int8_t v) { return v < 0; }
static inline bool wid_negative_i16(int16_t v) { return v < 0; }
static inline bool wid_negative_i32(int32_t v) { return v < 0; }
static inline bool wid_negative_i64(int64_t v) { return v < 0; }
static inline bool wid_negative_u8(uint8_t v) { (void)v; return false; }
static inline bool wid_negative_u16(uint16_t v) { (void)v; return false; }
static inline bool wid_negative_u32(uint32_t v) { (void)v; return false; }
static inline bool wid_negative_u64(uint64_t v) { (void)v; return false; }

WID_INT_OPS(int8_t, i8, INT8_MIN)
WID_INT_OPS(int16_t, i16, INT16_MIN)
WID_INT_OPS(int32_t, i32, INT32_MIN)
WID_INT_OPS(int64_t, i64, INT64_MIN)
WID_INT_OPS(uint8_t, u8, 0)
WID_INT_OPS(uint16_t, u16, 0)
WID_INT_OPS(uint32_t, u32, 0)
WID_INT_OPS(uint64_t, u64, 0)

/* Checked negation (debug builds), for the signed types, which have it: `-MIN` overflows. */
#define WID_NEG_OP(T, S)                                                                             \
    static inline T wid_neg_##S(T a, wid_Location loc) {                                              \
        T r;                                                                                         \
        if (ckd_sub(&r, (T)0, a)) wid_panicf(loc, "integer overflow in unary `-`");                  \
        return r;                                                                                    \
    }

WID_NEG_OP(int8_t, i8)
WID_NEG_OP(int16_t, i16)
WID_NEG_OP(int32_t, i32)
WID_NEG_OP(int64_t, i64)

/* Shifts by at least the bit width produce 0 (or -1 for negative values shifted right). */
#define WID_SHIFT_OPS(T, U, S, BITS)                                                                 \
    static inline T wid_shl_##S(T a, uint64_t b) { return b >= BITS ? (T)0 : (T)((U)a << b); }      \
    static inline T wid_shr_##S(T a, uint64_t b) {                                                   \
        if (b >= BITS) return (T)(a < 0 ? -1 : 0);                                                   \
        return (T)(a >> b);                                                                          \
    }

WID_SHIFT_OPS(int8_t, uint8_t, i8, 8)
WID_SHIFT_OPS(int16_t, uint16_t, i16, 16)
WID_SHIFT_OPS(int32_t, uint32_t, i32, 32)
WID_SHIFT_OPS(int64_t, uint64_t, i64, 64)

#define WID_USHIFT_OPS(T, S, BITS)                                                                   \
    static inline T wid_shl_##S(T a, uint64_t b) { return b >= BITS ? (T)0 : (T)(a << b); }         \
    static inline T wid_shr_##S(T a, uint64_t b) { return b >= BITS ? (T)0 : (T)(a >> b); }

WID_USHIFT_OPS(uint8_t, u8, 8)
WID_USHIFT_OPS(uint16_t, u16, 16)
WID_USHIFT_OPS(uint32_t, u32, 32)
WID_USHIFT_OPS(uint64_t, u64, 64)

/*
 * A signed shift amount, checked in debug builds: a negative one panics.
 * Release builds convert it to `uint64_t` unchecked, so a negative amount is
 * past the width and shifts every bit out.
 */
static inline uint64_t wid_shift_amount(int64_t b, wid_Location loc) {
    if (b < 0) wid_panicf(loc, "shift by a negative amount: %lld", (long long)b);
    return (uint64_t)b;
}

/** Converts a float to an integer, saturating at the integer's range; NaN becomes 0. */
static inline int64_t wid_f2i(double v, int64_t lo, int64_t hi) {
    if (isnan(v)) return 0;
    if (v <= (double)lo) return lo;
    if (v >= (double)hi) return hi;
    return (int64_t)v;
}

/** Converts a float to an unsigned integer, saturating; NaN and negatives become 0. */
static inline uint64_t wid_f2u(double v, uint64_t hi) {
    if (isnan(v) || v <= 0.0) return 0;
    if (v >= (double)hi) return hi;
    return (uint64_t)v;
}

/* ----- memory --------------------------------------------------------------- */

/** Allocates zeroed memory with `a`, panicking when it fails. */
void *wid_alloc(wid_Allocator a, wid_Int size, wid_Int align, wid_Location loc);
/** Allocates `count` zeroed elements, panicking on overflow or failure. */
void *wid_alloc_array(wid_Allocator a, wid_Int count, wid_Int size, wid_Int align, wid_Location loc);
/** Releases memory obtained from `a`. */
void wid_free(wid_Allocator a, void *ptr, wid_Int size, wid_Location loc);
/** Releases everything `a` handed out, if it supports that. */
void wid_free_all(wid_Allocator a, wid_Location loc);
/** Grows or shrinks a block, preserving its contents. */
void *wid_resize(wid_Allocator a, void *ptr, wid_Int old_size, wid_Int new_size, wid_Int align, wid_Location loc);

/** The process-wide heap allocator backed by malloc. */
wid_Allocator wid_heap_allocator(void);
/** The calling thread's temporary arena. */
wid_Allocator wid_temp_allocator(void);

/* ----- writers ------------------------------------------------------------ */

/** The writer for standard output. */
wid_Writer *wid_stdout(void);
/** The writer for standard error. */
wid_Writer *wid_stderr(void);
/** Starts an empty string builder that allocates from `a`. */
static inline wid_Writer wid_builder(wid_Allocator a) { return (wid_Writer){nullptr, nullptr, 0, 0, a}; }
/** Returns the text a builder holds; it stays owned by the builder's allocator. */
static inline wid_String wid_builder_string(const wid_Writer *w) { return (wid_String){w->buf, w->len}; }

void wid_w_bytes(wid_Writer *w, const void *data, wid_Int len);
void wid_w_str(wid_Writer *w, wid_String s);
void wid_w_cstr(wid_Writer *w, const char *s);
void wid_w_char(wid_Writer *w, char c);
void wid_w_int(wid_Writer *w, int64_t v);
void wid_w_uint(wid_Writer *w, uint64_t v);
void wid_w_f64(wid_Writer *w, double v);
void wid_w_f32(wid_Writer *w, float v);
void wid_w_bool(wid_Writer *w, bool v);
void wid_w_rune(wid_Writer *w, wid_Rune r);
void wid_w_ptr(wid_Writer *w, const void *p);
/** Writes a string as a quoted, escaped literal. */
void wid_w_str_inspect(wid_Writer *w, wid_String s);
/** Writes a rune as a quoted literal. */
void wid_w_rune_inspect(wid_Writer *w, wid_Rune r);

/* ----- strings ------------------------------------------------------------ */

/** Returns true when both strings have the same bytes. */
static inline bool wid_string_eq(wid_String a, wid_String b) {
    return a.len == b.len && (a.len == 0 || memcmp(a.data, b.data, (size_t)a.len) == 0);
}

/** Compares strings byte-wise: negative, zero or positive. */
int wid_string_cmp(wid_String a, wid_String b);

/** Wraps a NUL-terminated C string without copying. */
static inline wid_String wid_string_from_cstring(const char *s) {
    return (wid_String){(const uint8_t *)(s ? s : ""), s ? (wid_Int)strlen(s) : 0};
}

/** Copies a string into a NUL-terminated C string allocated from `a`. */
char *wid_string_to_cstring(wid_String s, wid_Allocator a, wid_Location loc);
/** Decodes the rune at byte `i`; stores it in `*out` and returns its width. */
wid_Int wid_utf8_decode(wid_String s, wid_Int i, wid_Rune *out);
/** Returns the byte offset of `needle` in `s`, or -1. */
wid_Int wid_string_find(wid_String s, wid_String needle);
/** Returns the number of runes in `s`. */
wid_Int wid_string_rune_count(wid_String s);

/* ----- containers ----------------------------------------------------------- */

/** Ensures room for `cap` elements; a zero allocator falls back to `fallback`. */
void wid_dyn_reserve(wid_RawDyn *d, wid_Int cap, wid_Int size, wid_Int align, wid_Allocator fallback, wid_Location loc);
/** Appends one zeroed element and returns a pointer to it. */
void *wid_dyn_push(wid_RawDyn *d, wid_Int size, wid_Int align, wid_Allocator fallback, wid_Location loc);
/** Inserts one zeroed element at `index` and returns a pointer to it. */
void *wid_dyn_insert(wid_RawDyn *d, wid_Int index, wid_Int size, wid_Int align, wid_Allocator fallback, wid_Location loc);
/** Removes the element at `index`, shifting later elements down. */
void wid_dyn_remove(wid_RawDyn *d, wid_Int index, wid_Int size, wid_Location loc);
/** Releases the storage of a dynamic array and empties it. */
void wid_dyn_free(wid_RawDyn *d, wid_Int size, wid_Location loc);

/** Returns a pointer to the value stored under `key`, or nullptr. */
void *wid_map_find(const wid_RawMap *m, const wid_MapInfo *info, const void *key);
/** Returns a pointer to the value slot for `key`, inserting a zeroed one when missing. */
void *wid_map_put(wid_RawMap *m, const wid_MapInfo *info, const void *key, wid_Allocator fallback, wid_Location loc);
/** Removes `key`; returns true when it was present. */
bool wid_map_remove(wid_RawMap *m, const wid_MapInfo *info, const void *key);
/** Releases the storage of a map and empties it. */
void wid_map_free(wid_RawMap *m, const wid_MapInfo *info, wid_Location loc);
/** Returns the index of the next occupied slot at or after `i`, or -1. */
wid_Int wid_map_next(const wid_RawMap *m, const wid_MapInfo *info, wid_Int i);
/** Returns a pointer to the key in slot `i`. */
static inline void *wid_map_key(const wid_RawMap *m, const wid_MapInfo *info, wid_Int i) {
    return m->slots + i * info->slot_size + info->key_offset;
}
/** Returns a pointer to the value in slot `i`. */
static inline void *wid_map_value(const wid_RawMap *m, const wid_MapInfo *info, wid_Int i) {
    return m->slots + i * info->slot_size + info->value_offset;
}

/** Appends `count` elements copied from `src`, which may point into `d` itself. */
void wid_dyn_append(wid_RawDyn *d, const void *src, wid_Int count, wid_Int size, wid_Int align, wid_Allocator fallback,
                    wid_Location loc);
/** Sets the length to `len`, zero-filling new elements. */
void wid_dyn_resize(wid_RawDyn *d, wid_Int len, wid_Int size, wid_Int align, wid_Allocator fallback, wid_Location loc);

/* ----- core library support ------------------------------------------------ */
/*
 * Functions `core:` packages reach through `@[extern("wid_…")]`. The compiler
 * emits no prototype for `wid_` externs, so these declarations are the ones the
 * C compiler checks calls against.
 */

/** Copies `n` bytes; the ranges may overlap. */
void wid_mem_copy(void *dst, const void *src, wid_Int n);
/** Sets `n` bytes to `byte`. */
void wid_mem_set(void *dst, uint8_t byte, wid_Int n);
/** Compares `n` bytes: negative, zero or positive. */
wid_Int wid_mem_compare(const void *a, const void *b, wid_Int n);

/** Creates a tracking allocator over `backing`; its own tables use malloc. */
void *wid_tracker_new(wid_Allocator backing);
/** Destroys a tracker. Blocks it still tracks are not freed. */
void wid_tracker_delete(void *tracker);
/** Returns the allocator that records every block handed out through `tracker`. */
wid_Allocator wid_tracker_allocator(void *tracker);
/** Snapshots the live blocks in source order and returns how many there are. */
wid_Int wid_tracker_leaks(void *tracker);
/** The size of leak `i` of the last snapshot. */
wid_Int wid_tracker_leak_size(void *tracker, wid_Int i);
/** Where leak `i` of the last snapshot was allocated. */
wid_Location wid_tracker_leak_location(void *tracker, wid_Int i);
/** Bytes currently allocated through the tracker. */
wid_Int wid_tracker_bytes(void *tracker);
/** Prints the live blocks to stderr and returns how many there are. */
wid_Int wid_tracker_report(void *tracker);

/** Error kinds reported by the `wid_os_` functions; `core:os` maps them to `Error` symbols. */
enum : int32_t {
    WID_OS_OK = 0,
    WID_OS_NOT_FOUND = 1,
    WID_OS_PERMISSION_DENIED = 2,
    WID_OS_EXISTS = 3,
    WID_OS_IS_DIRECTORY = 4,
    WID_OS_NOT_DIRECTORY = 5,
    WID_OS_NO_SPACE = 6,
    WID_OS_INVALID = 7,
    WID_OS_IO = 8,
    WID_OS_END = -1,
};

/** The command-line arguments, program name first; `wid_arg_count()` of them. */
wid_String *wid_os_args(void);
/** Reads a whole file into a block from `a`, stored in `*data` and `*len`. */
int32_t wid_os_read_file(wid_String path, wid_Allocator a, uint8_t **data, wid_Int *len, wid_Location loc);
/** Writes (or appends) `data` to a file, creating it when missing. */
int32_t wid_os_write_file(wid_String path, wid_String data, bool append);
/** Removes a file or an empty directory. */
int32_t wid_os_remove(wid_String path);
/** Creates a directory. */
int32_t wid_os_make_dir(wid_String path);
/** Returns true when `path` names an existing file or directory. */
bool wid_os_exists(wid_String path);
/** Returns true when `path` names a directory. */
bool wid_os_is_dir(wid_String path);
/** Looks up an environment variable; `*found` says whether it is set. */
wid_String wid_os_getenv(wid_String name, bool *found);
/** Reads one line from standard input without its line ending; WID_OS_END at the end. */
int32_t wid_os_read_line(wid_Allocator a, uint8_t **data, wid_Int *len, wid_Location loc);
/** Writes text to standard error. */
void wid_os_write_stderr(wid_String s);
/** Flushes output and ends the process with `code`. */
[[noreturn]] void wid_os_exit(int32_t code);

/**
 * Formats `v` into `buf` (at most `cap` bytes) and returns the length:
 * style 'f' fixed, 'e' scientific, 'g' shortest of the two. A negative
 * `precision` prints the shortest digits that round-trip.
 */
wid_Int wid_fmt_float(double v, wid_Int precision, uint8_t style, uint8_t *buf, wid_Int cap);
/** Parses a whole string as a float; returns false when it is not one. */
bool wid_parse_float(wid_String s, double *out);

#endif /* WID_RUNTIME_H */

/* ========================================================================== */

#ifdef WID_RUNTIME_IMPLEMENTATION
#ifndef WID_RUNTIME_IMPLEMENTED
#define WID_RUNTIME_IMPLEMENTED

static int wid_argc_;
static char **wid_argv_;

static wid_String *wid_os_args_;

#ifdef WID_DEBUG
static void *wid_debug_tracker_;
#endif

void wid_runtime_init(int argc, char **argv) {
    wid_argc_ = argc;
    wid_argv_ = argv;
    wid_os_args_ = calloc((size_t)(argc > 0 ? argc : 1), sizeof *wid_os_args_);
    if (!wid_os_args_) {
        fputs("wid: out of memory reading the command line\n", stderr);
        exit(101);
    }
    for (int i = 0; i < argc; i++) wid_os_args_[i] = wid_string_from_cstring(argv[i]);
    static char buffer[1 << 16];
    setvbuf(stdout, buffer, _IOFBF, sizeof buffer);
}

void wid_runtime_exit(void) {
    fflush(stdout);
#ifdef WID_DEBUG
    if (wid_debug_tracker_) wid_tracker_report(wid_debug_tracker_);
#endif
}

wid_String wid_arg(wid_Int i) {
    if (i < 0 || i >= wid_argc_) return (wid_String){(const uint8_t *)"", 0};
    return wid_string_from_cstring(wid_argv_[i]);
}

wid_Int wid_arg_count(void) { return wid_argc_; }

/* ----- panics ------------------------------------------------------------- */

static void wid_print_location_(wid_Location loc) {
    if (loc.file) {
        fprintf(stderr, "  at %s:%d:%d", loc.file, (int)loc.line, (int)loc.column);
        if (loc.proc) fprintf(stderr, " in `%s`", loc.proc);
        fputc('\n', stderr);
    }
}

/* Debug builds abort so a debugger stops at the panic; others exit with 101. */
[[noreturn]] static void wid_die_(void) {
    fflush(stderr);
#ifdef WID_DEBUG
    abort();
#else
    exit(101);
#endif
}

void wid_panic(wid_String message, wid_Location loc) {
    fflush(stdout);
    fputs("panic: ", stderr);
    fwrite(message.data, 1, (size_t)message.len, stderr);
    fputc('\n', stderr);
    wid_print_location_(loc);
    wid_die_();
}

void wid_panicf(wid_Location loc, const char *fmt, ...) {
    fflush(stdout);
    fputs("panic: ", stderr);
    va_list args;
    va_start(args, fmt);
    vfprintf(stderr, fmt, args);
    va_end(args);
    fputc('\n', stderr);
    wid_print_location_(loc);
    wid_die_();
}

void wid_nil_panic(wid_Location loc) { wid_panicf(loc, "called or unwrapped a nil value"); }

/* ----- memory --------------------------------------------------------------- */

void *wid_alloc(wid_Allocator a, wid_Int size, wid_Int align, wid_Location loc) {
    if (size < 0) wid_panicf(loc, "cannot allocate a negative size (%lld bytes)", (long long)size);
    if (size == 0) return nullptr;
    if (!a.proc) a = wid_heap_allocator();
    void *p = a.proc(a.data, WID_ALLOC, size, align, nullptr, 0, loc);
    if (!p) wid_panicf(loc, "out of memory allocating %lld bytes", (long long)size);
    return p;
}

void *wid_alloc_array(wid_Allocator a, wid_Int count, wid_Int size, wid_Int align, wid_Location loc) {
    wid_Int bytes;
    if (count < 0 || ckd_mul(&bytes, count, size)) {
        wid_panicf(loc, "cannot allocate %lld elements of %lld bytes", (long long)count, (long long)size);
    }
    return wid_alloc(a, bytes, align, loc);
}

void wid_free(wid_Allocator a, void *ptr, wid_Int size, wid_Location loc) {
    if (!ptr) return;
    if (!a.proc) a = wid_heap_allocator();
    a.proc(a.data, WID_FREE, 0, 0, ptr, size, loc);
}

void wid_free_all(wid_Allocator a, wid_Location loc) {
    if (a.proc) a.proc(a.data, WID_FREE_ALL, 0, 0, nullptr, 0, loc);
}

void *wid_resize(wid_Allocator a, void *ptr, wid_Int old_size, wid_Int new_size, wid_Int align, wid_Location loc) {
    if (new_size == 0) {
        wid_free(a, ptr, old_size, loc);
        return nullptr;
    }
    if (!a.proc) a = wid_heap_allocator();
    void *p = a.proc(a.data, WID_RESIZE, new_size, align, ptr, old_size, loc);
    if (!p) wid_panicf(loc, "out of memory resizing to %lld bytes", (long long)new_size);
    return p;
}

static void *wid_heap_proc_(void *data, wid_AllocMode mode, wid_Int size, wid_Int align, void *old, wid_Int old_size,
                            wid_Location loc) {
    (void)data;
    (void)loc;
    switch (mode) {
    case WID_ALLOC: {
        if (align <= 16) return calloc(1, (size_t)size);
        size_t rounded = ((size_t)size + (size_t)align - 1) / (size_t)align * (size_t)align;
        void *p = aligned_alloc((size_t)align, rounded);
        if (p) memset(p, 0, rounded);
        return p;
    }
    case WID_FREE:
        free(old);
        return nullptr;
    case WID_FREE_ALL:
        return nullptr;
    case WID_RESIZE: {
        if (align <= 16) {
            void *p = realloc(old, (size_t)size);
            if (p && size > old_size) memset((char *)p + old_size, 0, (size_t)(size - old_size));
            return p;
        }
        void *p = wid_heap_proc_(data, WID_ALLOC, size, align, nullptr, 0, loc);
        if (p && old) {
            memcpy(p, old, (size_t)(old_size < size ? old_size : size));
            free(old);
        }
        return p;
    }
    }
    return nullptr;
}

wid_Allocator wid_heap_allocator(void) { return (wid_Allocator){wid_heap_proc_, nullptr}; }

/* A growable arena made of chained blocks. */
typedef struct wid_ArenaBlock_ {
    struct wid_ArenaBlock_ *prev;
    size_t used;
    size_t cap;
    alignas(16) unsigned char data[];
} wid_ArenaBlock_;

typedef struct wid_Arena_ {
    wid_ArenaBlock_ *block;
    size_t min_block;
} wid_Arena_;

static void *wid_arena_alloc_(wid_Arena_ *arena, size_t size, size_t align) {
    if (align < 1) align = 1;
    wid_ArenaBlock_ *b = arena->block;
    if (b) {
        size_t start = (b->used + align - 1) / align * align;
        if (start + size <= b->cap) {
            b->used = start + size;
            memset(b->data + start, 0, size);
            return b->data + start;
        }
    }
    size_t cap = arena->min_block;
    while (cap < size + align) cap *= 2;
    wid_ArenaBlock_ *nb = malloc(sizeof *nb + cap);
    if (!nb) return nullptr;
    nb->prev = b;
    nb->cap = cap;
    size_t start = ((uintptr_t)nb->data % align) ? (align - (uintptr_t)nb->data % align) : 0;
    nb->used = start + size;
    arena->block = nb;
    memset(nb->data + start, 0, size);
    return nb->data + start;
}

static void wid_arena_reset_(wid_Arena_ *arena) {
    wid_ArenaBlock_ *b = arena->block;
    while (b && b->prev) {
        wid_ArenaBlock_ *prev = b->prev;
        free(b);
        b = prev;
    }
    if (b) b->used = 0;
    arena->block = b;
}

static void *wid_arena_proc_(void *data, wid_AllocMode mode, wid_Int size, wid_Int align, void *old, wid_Int old_size,
                             wid_Location loc) {
    (void)loc;
    wid_Arena_ *arena = data;
    switch (mode) {
    case WID_ALLOC:
        return wid_arena_alloc_(arena, (size_t)size, (size_t)align);
    case WID_FREE:
        return nullptr;
    case WID_FREE_ALL:
        wid_arena_reset_(arena);
        return nullptr;
    case WID_RESIZE: {
        void *p = wid_arena_alloc_(arena, (size_t)size, (size_t)align);
        if (p && old) memcpy(p, old, (size_t)(old_size < size ? old_size : size));
        return p;
    }
    }
    return nullptr;
}

static thread_local wid_Arena_ wid_temp_arena_ = {nullptr, 64 * 1024};

wid_Allocator wid_temp_allocator(void) { return (wid_Allocator){wid_arena_proc_, &wid_temp_arena_}; }

static void wid_default_log_(void *data, int32_t level, wid_String message, wid_Location loc) {
    (void)data;
    static const char *const names[] = {"debug", "info", "warn", "error", "fatal"};
    const char *name = level >= 0 && level < 5 ? names[level] : "log";
    fprintf(stderr, "[%s] %.*s", name, (int)message.len, (const char *)message.data);
    if (loc.file) fprintf(stderr, " (%s:%d)", loc.file, (int)loc.line);
    fputc('\n', stderr);
}

/* Debug builds wrap the heap in one process-wide tracking allocator, which
 * reports leaks when `main` returns. */
static wid_Allocator wid_default_allocator_(void) {
#ifdef WID_DEBUG
    if (!wid_debug_tracker_) wid_debug_tracker_ = wid_tracker_new(wid_heap_allocator());
    return wid_tracker_allocator(wid_debug_tracker_);
#else
    return wid_heap_allocator();
#endif
}

wid_Context wid_default_context(void) {
    return (wid_Context){
        .allocator = wid_default_allocator_(),
        .temp_allocator = wid_temp_allocator(),
        .logger = {wid_default_log_, nullptr},
        .user_data = nullptr,
        .user_index = 0,
    };
}

/* ----- writers ------------------------------------------------------------ */

wid_Writer *wid_stdout(void) {
    static wid_Writer w;
    w.file = stdout;
    return &w;
}

wid_Writer *wid_stderr(void) {
    static wid_Writer w;
    w.file = stderr;
    return &w;
}

void wid_w_bytes(wid_Writer *w, const void *data, wid_Int len) {
    if (len <= 0) return;
    if (w->file) {
        fwrite(data, 1, (size_t)len, w->file);
        return;
    }
    if (w->len + len > w->cap) {
        wid_Int cap = w->cap ? w->cap : 64;
        while (cap < w->len + len) cap *= 2;
        w->buf = wid_resize(w->allocator, w->buf, w->cap, cap, 1, (wid_Location){});
        w->cap = cap;
    }
    memcpy(w->buf + w->len, data, (size_t)len);
    w->len += len;
}

void wid_w_str(wid_Writer *w, wid_String s) { wid_w_bytes(w, s.data, s.len); }

void wid_w_cstr(wid_Writer *w, const char *s) {
    if (s) wid_w_bytes(w, s, (wid_Int)strlen(s));
}

void wid_w_char(wid_Writer *w, char c) { wid_w_bytes(w, &c, 1); }

void wid_w_int(wid_Writer *w, int64_t v) {
    char buf[32];
    int n = snprintf(buf, sizeof buf, "%lld", (long long)v);
    wid_w_bytes(w, buf, n);
}

void wid_w_uint(wid_Writer *w, uint64_t v) {
    char buf[32];
    int n = snprintf(buf, sizeof buf, "%llu", (unsigned long long)v);
    wid_w_bytes(w, buf, n);
}

/*
 * Formats a finite float the way Ruby prints it: the shortest digits that
 * round-trip, in fixed notation for exponents from -4 to 14 and scientific
 * notation otherwise, always with a decimal point.
 */
static void wid_format_float_(double v, bool single, char *out, size_t n) {
    char buf[64];
    int max = single ? 9 : 17;
    int prec = 0;
    for (; prec < max; prec++) {
        snprintf(buf, sizeof buf, "%.*e", prec, v);
        if (single ? strtof(buf, nullptr) == (float)v : strtod(buf, nullptr) == v) break;
    }
    const char *e = strchr(buf, 'e');
    int exp = e ? atoi(e + 1) : 0;
    if (exp >= -4 && exp < 15) {
        int decimals = prec - exp;
        snprintf(out, n, "%.*f", decimals < 1 ? 1 : decimals, v);
        return;
    }
    if (prec == 0 && e) {
        snprintf(out, n, "%.*s.0%s", (int)(e - buf), buf, e);
    } else {
        snprintf(out, n, "%s", buf);
    }
}

static void wid_w_float_(wid_Writer *w, double v, bool single) {
    if (isnan(v)) {
        wid_w_cstr(w, "NaN");
        return;
    }
    if (isinf(v)) {
        wid_w_cstr(w, v < 0 ? "-Infinity" : "Infinity");
        return;
    }
    char buf[400];
    wid_format_float_(v, single, buf, sizeof buf);
    wid_w_cstr(w, buf);
}

void wid_w_f64(wid_Writer *w, double v) { wid_w_float_(w, v, false); }

void wid_w_f32(wid_Writer *w, float v) { wid_w_float_(w, (double)v, true); }

void wid_w_bool(wid_Writer *w, bool v) { wid_w_cstr(w, v ? "true" : "false"); }

void wid_w_rune(wid_Writer *w, wid_Rune r) {
    uint32_t c = (uint32_t)r;
    char buf[4];
    int n;
    if (c < 0x80) {
        buf[0] = (char)c;
        n = 1;
    } else if (c < 0x800) {
        buf[0] = (char)(0xC0 | (c >> 6));
        buf[1] = (char)(0x80 | (c & 0x3F));
        n = 2;
    } else if (c < 0x10000) {
        buf[0] = (char)(0xE0 | (c >> 12));
        buf[1] = (char)(0x80 | ((c >> 6) & 0x3F));
        buf[2] = (char)(0x80 | (c & 0x3F));
        n = 3;
    } else {
        buf[0] = (char)(0xF0 | (c >> 18));
        buf[1] = (char)(0x80 | ((c >> 12) & 0x3F));
        buf[2] = (char)(0x80 | ((c >> 6) & 0x3F));
        buf[3] = (char)(0x80 | (c & 0x3F));
        n = 4;
    }
    wid_w_bytes(w, buf, n);
}

void wid_w_ptr(wid_Writer *w, const void *p) {
    if (!p) {
        wid_w_cstr(w, "nil");
        return;
    }
    char buf[32];
    int n = snprintf(buf, sizeof buf, "0x%llx", (unsigned long long)(uintptr_t)p);
    wid_w_bytes(w, buf, n);
}

static void wid_w_escaped_byte_(wid_Writer *w, uint8_t c, char quote) {
    switch (c) {
    case '\n': wid_w_cstr(w, "\\n"); break;
    case '\t': wid_w_cstr(w, "\\t"); break;
    case '\r': wid_w_cstr(w, "\\r"); break;
    case '\\': wid_w_cstr(w, "\\\\"); break;
    case 0x1B: wid_w_cstr(w, "\\e"); break;
    case 0: wid_w_cstr(w, "\\0"); break;
    default:
        if (c == (uint8_t)quote) {
            wid_w_char(w, '\\');
            wid_w_char(w, (char)c);
        } else if (c < 0x20 || c == 0x7F) {
            char buf[8];
            int n = snprintf(buf, sizeof buf, "\\x%02X", c);
            wid_w_bytes(w, buf, n);
        } else {
            wid_w_char(w, (char)c);
        }
    }
}

void wid_w_str_inspect(wid_Writer *w, wid_String s) {
    wid_w_char(w, '"');
    for (wid_Int i = 0; i < s.len; i++) {
        uint8_t c = s.data[i];
        if (c == '#' && i + 1 < s.len && s.data[i + 1] == '{') {
            wid_w_cstr(w, "\\#");
            continue;
        }
        wid_w_escaped_byte_(w, c, '"');
    }
    wid_w_char(w, '"');
}

void wid_w_rune_inspect(wid_Writer *w, wid_Rune r) {
    wid_w_char(w, '\'');
    if (r >= 0 && r < 0x80) {
        wid_w_escaped_byte_(w, (uint8_t)r, '\'');
    } else {
        wid_w_rune(w, r);
    }
    wid_w_char(w, '\'');
}

/* ----- strings ------------------------------------------------------------ */

int wid_string_cmp(wid_String a, wid_String b) {
    wid_Int n = a.len < b.len ? a.len : b.len;
    int c = n > 0 ? memcmp(a.data, b.data, (size_t)n) : 0;
    if (c != 0) return c;
    return a.len < b.len ? -1 : (a.len > b.len ? 1 : 0);
}

char *wid_string_to_cstring(wid_String s, wid_Allocator a, wid_Location loc) {
    char *out = wid_alloc(a, s.len + 1, 1, loc);
    if (s.len > 0) memcpy(out, s.data, (size_t)s.len);
    out[s.len] = '\0';
    return out;
}

wid_Int wid_utf8_decode(wid_String s, wid_Int i, wid_Rune *out) {
    const uint8_t *p = s.data + i;
    wid_Int left = s.len - i;
    uint8_t c = p[0];
    if (c < 0x80) {
        *out = c;
        return 1;
    }
    wid_Int width = (c & 0xE0) == 0xC0 ? 2 : (c & 0xF0) == 0xE0 ? 3 : (c & 0xF8) == 0xF0 ? 4 : 0;
    if (width == 0 || width > left) {
        *out = 0xFFFD;
        return 1;
    }
    uint32_t value = c & (0x7F >> width);
    for (wid_Int k = 1; k < width; k++) {
        if ((p[k] & 0xC0) != 0x80) {
            *out = 0xFFFD;
            return 1;
        }
        value = (value << 6) | (p[k] & 0x3F);
    }
    *out = (wid_Rune)value;
    return width;
}

wid_Int wid_string_find(wid_String s, wid_String needle) {
    if (needle.len == 0) return 0;
    for (wid_Int i = 0; i + needle.len <= s.len; i++) {
        if (memcmp(s.data + i, needle.data, (size_t)needle.len) == 0) return i;
    }
    return -1;
}

wid_Int wid_string_rune_count(wid_String s) {
    wid_Int count = 0;
    for (wid_Int i = 0; i < s.len;) {
        wid_Rune r;
        i += wid_utf8_decode(s, i, &r);
        count++;
    }
    return count;
}

/* ----- dynamic arrays ------------------------------------------------------- */

void wid_dyn_reserve(wid_RawDyn *d, wid_Int cap, wid_Int size, wid_Int align, wid_Allocator fallback, wid_Location loc) {
    if (cap <= d->cap) return;
    if (!d->allocator.proc) d->allocator = fallback;
    wid_Int old_bytes, new_bytes;
    if (ckd_mul(&old_bytes, d->cap, size) || ckd_mul(&new_bytes, cap, size)) {
        wid_panicf(loc, "dynamic array of %lld elements is too large", (long long)cap);
    }
    d->data = wid_resize(d->allocator, d->data, old_bytes, new_bytes, align, loc);
    d->cap = cap;
}

void *wid_dyn_push(wid_RawDyn *d, wid_Int size, wid_Int align, wid_Allocator fallback, wid_Location loc) {
    if (d->len == d->cap) wid_dyn_reserve(d, d->cap ? d->cap * 2 : 8, size, align, fallback, loc);
    void *slot = (char *)d->data + d->len * size;
    memset(slot, 0, (size_t)size);
    d->len++;
    return slot;
}

void *wid_dyn_insert(wid_RawDyn *d, wid_Int index, wid_Int size, wid_Int align, wid_Allocator fallback, wid_Location loc) {
    if (index < 0 || index > d->len) {
        wid_panicf(loc, "insert position %lld is out of bounds for length %lld", (long long)index, (long long)d->len);
    }
    if (d->len == d->cap) wid_dyn_reserve(d, d->cap ? d->cap * 2 : 8, size, align, fallback, loc);
    char *base = d->data;
    memmove(base + (index + 1) * size, base + index * size, (size_t)((d->len - index) * size));
    memset(base + index * size, 0, (size_t)size);
    d->len++;
    return base + index * size;
}

void wid_dyn_remove(wid_RawDyn *d, wid_Int index, wid_Int size, wid_Location loc) {
    wid_bounds(index, d->len, loc);
    char *base = d->data;
    memmove(base + index * size, base + (index + 1) * size, (size_t)((d->len - index - 1) * size));
    d->len--;
}

void wid_dyn_free(wid_RawDyn *d, wid_Int size, wid_Location loc) {
    if (d->data) wid_free(d->allocator, d->data, d->cap * size, loc);
    d->data = nullptr;
    d->len = 0;
    d->cap = 0;
}

/* ----- maps ----------------------------------------------------------------- */

enum : uint8_t { WID_SLOT_EMPTY = 0, WID_SLOT_FULL = 1 };

static uint64_t wid_hash_bytes_(const void *data, wid_Int len) {
    uint64_t h = wid_hash_feed(0xcbf29ce484222325ull, data, len);
    return h ? h : 1;
}

/* A hash is never 0. */
static uint64_t wid_map_hash_(const wid_MapInfo *info, const void *key) {
    if (info->key_hash) {
        uint64_t h = info->key_hash(key);
        return h ? h : 1;
    }
    if (info->string_key) {
        const wid_String *s = key;
        return wid_hash_bytes_(s->data, s->len);
    }
    return wid_hash_bytes_(key, info->key_size);
}

static bool wid_map_key_eq_(const wid_MapInfo *info, const void *a, const void *b) {
    if (info->key_eq) return info->key_eq(a, b);
    if (info->string_key) return wid_string_eq(*(const wid_String *)a, *(const wid_String *)b);
    return memcmp(a, b, (size_t)info->key_size) == 0;
}

static uint8_t *wid_map_slot_(const wid_RawMap *m, const wid_MapInfo *info, wid_Int i) {
    return m->slots + i * info->slot_size;
}

/*
 * Open addressing with linear probing and no tombstones: removal shifts the
 * following entries of the cluster back, so lookups never scan dead slots.
 * Returns the slot holding `key`, or the empty slot where it would go.
 */
static wid_Int wid_map_probe_(const wid_RawMap *m, const wid_MapInfo *info, const void *key, uint64_t hash, bool *found) {
    wid_Int mask = m->cap - 1;
    wid_Int i = (wid_Int)(hash & (uint64_t)mask);
    for (wid_Int n = 0; n < m->cap; n++, i = (i + 1) & mask) {
        uint8_t *slot = wid_map_slot_(m, info, i);
        if (slot[0] == WID_SLOT_EMPTY) {
            *found = false;
            return i;
        }
        uint64_t slot_hash;
        memcpy(&slot_hash, slot + 8, sizeof slot_hash);
        if (slot_hash == hash && wid_map_key_eq_(info, slot + info->key_offset, key)) {
            *found = true;
            return i;
        }
    }
    *found = false;
    return -1;
}

void *wid_map_find(const wid_RawMap *m, const wid_MapInfo *info, const void *key) {
    if (m->len == 0) return nullptr;
    bool found;
    wid_Int i = wid_map_probe_(m, info, key, wid_map_hash_(info, key), &found);
    return found ? wid_map_slot_(m, info, i) + info->value_offset : nullptr;
}

static void wid_map_grow_(wid_RawMap *m, const wid_MapInfo *info, wid_Location loc) {
    wid_RawMap old = *m;
    wid_Int cap = old.cap ? old.cap * 2 : 8;
    m->slots = wid_alloc_array(m->allocator, cap, info->slot_size, info->align, loc);
    m->cap = cap;
    m->len = 0;
    for (wid_Int i = 0; i < old.cap; i++) {
        uint8_t *slot = old.slots + i * info->slot_size;
        if (slot[0] != WID_SLOT_FULL) continue;
        uint64_t hash;
        memcpy(&hash, slot + 8, sizeof hash);
        bool found;
        wid_Int j = wid_map_probe_(m, info, slot + info->key_offset, hash, &found);
        memcpy(wid_map_slot_(m, info, j), slot, (size_t)info->slot_size);
        m->len++;
    }
    if (old.slots) wid_free(m->allocator, old.slots, old.cap * info->slot_size, loc);
}

void *wid_map_put(wid_RawMap *m, const wid_MapInfo *info, const void *key, wid_Allocator fallback, wid_Location loc) {
    if (!m->allocator.proc) m->allocator = fallback;
    if ((m->len + 1) * 4 > m->cap * 3) wid_map_grow_(m, info, loc);
    uint64_t hash = wid_map_hash_(info, key);
    bool found;
    wid_Int i = wid_map_probe_(m, info, key, hash, &found);
    uint8_t *slot = wid_map_slot_(m, info, i);
    if (!found) {
        memset(slot, 0, (size_t)info->slot_size);
        slot[0] = WID_SLOT_FULL;
        memcpy(slot + 8, &hash, sizeof hash);
        memcpy(slot + info->key_offset, key, (size_t)info->key_size);
        m->len++;
    }
    return slot + info->value_offset;
}

bool wid_map_remove(wid_RawMap *m, const wid_MapInfo *info, const void *key) {
    if (m->len == 0) return false;
    bool found;
    wid_Int i = wid_map_probe_(m, info, key, wid_map_hash_(info, key), &found);
    if (!found) return false;
    wid_Int mask = m->cap - 1;
    wid_Int j = i;
    for (;;) {
        wid_map_slot_(m, info, i)[0] = WID_SLOT_EMPTY;
        for (;;) {
            j = (j + 1) & mask;
            uint8_t *slot = wid_map_slot_(m, info, j);
            if (slot[0] == WID_SLOT_EMPTY) {
                m->len--;
                return true;
            }
            uint64_t hash;
            memcpy(&hash, slot + 8, sizeof hash);
            wid_Int ideal = (wid_Int)(hash & (uint64_t)mask);
            /* Move the entry back only if its ideal slot is not between i and j. */
            bool between = i <= j ? (i < ideal && ideal <= j) : (i < ideal || ideal <= j);
            if (!between) break;
        }
        memcpy(wid_map_slot_(m, info, i), wid_map_slot_(m, info, j), (size_t)info->slot_size);
        i = j;
    }
}

void wid_map_free(wid_RawMap *m, const wid_MapInfo *info, wid_Location loc) {
    if (m->slots) wid_free(m->allocator, m->slots, m->cap * info->slot_size, loc);
    m->slots = nullptr;
    m->len = 0;
    m->cap = 0;
}

wid_Int wid_map_next(const wid_RawMap *m, const wid_MapInfo *info, wid_Int i) {
    for (; i < m->cap; i++) {
        if (wid_map_slot_(m, info, i)[0] == WID_SLOT_FULL) return i;
    }
    return -1;
}

void wid_dyn_append(wid_RawDyn *d, const void *src, wid_Int count, wid_Int size, wid_Int align, wid_Allocator fallback,
                    wid_Location loc) {
    if (count <= 0) return;
    wid_Int need;
    if (ckd_add(&need, d->len, count)) wid_panicf(loc, "dynamic array length overflows");
    const char *base = d->data;
    bool inside = base && (const char *)src >= base && (const char *)src < base + d->len * size;
    wid_Int offset = inside ? (wid_Int)((const char *)src - base) : 0;
    if (need > d->cap) {
        wid_Int cap = d->cap ? d->cap : 8;
        while (cap < need) {
            if (ckd_mul(&cap, cap, 2)) wid_panicf(loc, "dynamic array of %lld elements is too large", (long long)need);
        }
        wid_dyn_reserve(d, cap, size, align, fallback, loc);
    }
    if (inside) src = (const char *)d->data + offset;
    memmove((char *)d->data + d->len * size, src, (size_t)(count * size));
    d->len = need;
}

void wid_dyn_resize(wid_RawDyn *d, wid_Int len, wid_Int size, wid_Int align, wid_Allocator fallback, wid_Location loc) {
    if (len < 0) wid_panicf(loc, "cannot resize a dynamic array to %lld elements", (long long)len);
    if (len > d->cap) wid_dyn_reserve(d, len, size, align, fallback, loc);
    if (len > d->len) memset((char *)d->data + d->len * size, 0, (size_t)((len - d->len) * size));
    d->len = len;
}

/* ----- core library support ------------------------------------------------ */

void wid_mem_copy(void *dst, const void *src, wid_Int n) {
    if (n > 0) memmove(dst, src, (size_t)n);
}

void wid_mem_set(void *dst, uint8_t byte, wid_Int n) {
    if (n > 0) memset(dst, byte, (size_t)n);
}

wid_Int wid_mem_compare(const void *a, const void *b, wid_Int n) { return n > 0 ? memcmp(a, b, (size_t)n) : 0; }

/* A tracking allocator: an open-addressing table of live blocks keyed by
 * address, plus a ring of recently freed blocks for double-free reports. */
typedef struct wid_TrackedBlock_ {
    void *ptr;
    wid_Int size;
    wid_Location loc;
} wid_TrackedBlock_;

typedef struct wid_Tracker_ {
    wid_Allocator backing;
    wid_TrackedBlock_ *slots;
    wid_Int cap;
    wid_Int len;
    wid_Int bytes;
    wid_TrackedBlock_ freed[32];
    wid_Location freed_at[32];
    wid_Int freed_next;
    wid_TrackedBlock_ *snapshot;
    wid_Int snapshot_len;
} wid_Tracker_;

static wid_Int wid_track_home_(const wid_Tracker_ *t, const void *ptr) {
    uint64_t h = (uint64_t)(uintptr_t)ptr * UINT64_C(0x9E3779B97F4A7C15);
    return (wid_Int)(h >> 32) & (t->cap - 1);
}

static wid_Int wid_track_find_(const wid_Tracker_ *t, const void *ptr) {
    if (!t->cap) return -1;
    for (wid_Int i = wid_track_home_(t, ptr);; i = (i + 1) & (t->cap - 1)) {
        if (!t->slots[i].ptr) return -1;
        if (t->slots[i].ptr == ptr) return i;
    }
}

static void wid_track_put_(wid_Tracker_ *t, wid_TrackedBlock_ block) {
    if ((t->len + 1) * 2 > t->cap) {
        wid_Int cap = t->cap ? t->cap * 2 : 64;
        wid_TrackedBlock_ *slots = calloc((size_t)cap, sizeof *slots);
        if (!slots) wid_panicf(block.loc, "out of memory tracking allocations");
        wid_TrackedBlock_ *old = t->slots;
        wid_Int old_cap = t->cap;
        t->slots = slots;
        t->cap = cap;
        for (wid_Int i = 0; i < old_cap; i++) {
            if (!old[i].ptr) continue;
            wid_Int j = wid_track_home_(t, old[i].ptr);
            while (t->slots[j].ptr) j = (j + 1) & (cap - 1);
            t->slots[j] = old[i];
        }
        free(old);
    }
    wid_Int i = wid_track_home_(t, block.ptr);
    while (t->slots[i].ptr) i = (i + 1) & (t->cap - 1);
    t->slots[i] = block;
    t->len++;
    t->bytes += block.size;
}

/* Removes slot `i`, shifting later entries of its cluster back. */
static void wid_track_remove_(wid_Tracker_ *t, wid_Int i) {
    t->bytes -= t->slots[i].size;
    t->len--;
    wid_Int mask = t->cap - 1;
    wid_Int j = i;
    for (;;) {
        t->slots[i].ptr = nullptr;
        for (;;) {
            j = (j + 1) & mask;
            if (!t->slots[j].ptr) return;
            wid_Int home = wid_track_home_(t, t->slots[j].ptr);
            bool between = i <= j ? (i < home && home <= j) : (i < home || home <= j);
            if (!between) break;
        }
        t->slots[i] = t->slots[j];
        i = j;
    }
}

/* Takes a block out of the table for a free or resize, panicking with both
 * sites when the pointer was already freed or never came from this tracker. */
static wid_TrackedBlock_ wid_track_take_(wid_Tracker_ *t, void *ptr, wid_Location loc, const char *what) {
    wid_Int i = wid_track_find_(t, ptr);
    if (i >= 0) {
        wid_TrackedBlock_ block = t->slots[i];
        wid_track_remove_(t, i);
        return block;
    }
    for (wid_Int k = 0; k < 32; k++) {
        const wid_TrackedBlock_ *f = &t->freed[k];
        if (f->ptr != ptr) continue;
        const wid_Location *at = &t->freed_at[k];
        wid_panicf(loc, "%s of memory that was already freed\n  %lld bytes allocated at %s:%d:%d\n  first freed at %s:%d:%d",
                   what, (long long)f->size, f->loc.file ? f->loc.file : "?", (int)f->loc.line, (int)f->loc.column,
                   at->file ? at->file : "?", (int)at->line, (int)at->column);
    }
    wid_panicf(loc, "%s of a pointer this allocator did not hand out (freed twice, or with the wrong allocator)", what);
}

static void wid_track_forget_(wid_Tracker_ *t, wid_TrackedBlock_ block, wid_Location loc) {
    t->freed[t->freed_next] = block;
    t->freed_at[t->freed_next] = loc;
    t->freed_next = (t->freed_next + 1) % 32;
}

static void *wid_tracker_proc_(void *data, wid_AllocMode mode, wid_Int size, wid_Int align, void *old, wid_Int old_size,
                               wid_Location loc) {
    wid_Tracker_ *t = data;
    wid_Allocator b = t->backing;
    switch (mode) {
    case WID_ALLOC: {
        void *p = b.proc(b.data, mode, size, align, old, old_size, loc);
        if (p) wid_track_put_(t, (wid_TrackedBlock_){p, size, loc});
        return p;
    }
    case WID_FREE:
        if (!old) return nullptr;
        wid_track_forget_(t, wid_track_take_(t, old, loc, "free"), loc);
        return b.proc(b.data, mode, size, align, old, old_size, loc);
    case WID_RESIZE: {
        wid_TrackedBlock_ previous = {};
        if (old) previous = wid_track_take_(t, old, loc, "resize");
        void *p = b.proc(b.data, mode, size, align, old, old_size, loc);
        if (p) {
            wid_track_put_(t, (wid_TrackedBlock_){p, size, old ? previous.loc : loc});
        } else if (old) {
            wid_track_put_(t, previous);
        }
        return p;
    }
    case WID_FREE_ALL:
        if (t->slots) memset(t->slots, 0, (size_t)t->cap * sizeof *t->slots);
        t->len = 0;
        t->bytes = 0;
        return b.proc(b.data, mode, size, align, old, old_size, loc);
    }
    return nullptr;
}

void *wid_tracker_new(wid_Allocator backing) {
    wid_Tracker_ *t = calloc(1, sizeof *t);
    if (!t) wid_panicf((wid_Location){}, "out of memory creating a tracking allocator");
    t->backing = backing.proc ? backing : wid_heap_allocator();
    return t;
}

void wid_tracker_delete(void *tracker) {
    wid_Tracker_ *t = tracker;
    if (!t) return;
    free(t->slots);
    free(t->snapshot);
    free(t);
}

wid_Allocator wid_tracker_allocator(void *tracker) { return (wid_Allocator){wid_tracker_proc_, tracker}; }

static int wid_track_order_(const void *pa, const void *pb) {
    const wid_TrackedBlock_ *a = pa;
    const wid_TrackedBlock_ *b = pb;
    int c = strcmp(a->loc.file ? a->loc.file : "", b->loc.file ? b->loc.file : "");
    if (c) return c;
    if (a->loc.line != b->loc.line) return a->loc.line < b->loc.line ? -1 : 1;
    if (a->loc.column != b->loc.column) return a->loc.column < b->loc.column ? -1 : 1;
    return a->size < b->size ? -1 : a->size > b->size;
}

wid_Int wid_tracker_leaks(void *tracker) {
    wid_Tracker_ *t = tracker;
    free(t->snapshot);
    t->snapshot = nullptr;
    t->snapshot_len = 0;
    if (!t->len) return 0;
    t->snapshot = malloc((size_t)t->len * sizeof *t->snapshot);
    if (!t->snapshot) return 0;
    for (wid_Int i = 0; i < t->cap; i++) {
        if (t->slots[i].ptr) t->snapshot[t->snapshot_len++] = t->slots[i];
    }
    qsort(t->snapshot, (size_t)t->snapshot_len, sizeof *t->snapshot, wid_track_order_);
    return t->snapshot_len;
}

wid_Int wid_tracker_leak_size(void *tracker, wid_Int i) {
    const wid_Tracker_ *t = tracker;
    return i >= 0 && i < t->snapshot_len ? t->snapshot[i].size : 0;
}

wid_Location wid_tracker_leak_location(void *tracker, wid_Int i) {
    const wid_Tracker_ *t = tracker;
    return i >= 0 && i < t->snapshot_len ? t->snapshot[i].loc : (wid_Location){};
}

wid_Int wid_tracker_bytes(void *tracker) { return ((const wid_Tracker_ *)tracker)->bytes; }

wid_Int wid_tracker_report(void *tracker) {
    wid_Tracker_ *t = tracker;
    wid_Int n = wid_tracker_leaks(t);
    if (!n) return 0;
    fflush(stdout);
    fprintf(stderr, "leak: %lld allocation%s (%lld bytes) never freed\n", (long long)n, n == 1 ? "" : "s",
            (long long)t->bytes);
    for (wid_Int i = 0; i < n; i++) {
        const wid_TrackedBlock_ *b = &t->snapshot[i];
        fprintf(stderr, "  %lld bytes allocated at %s:%d:%d", (long long)b->size, b->loc.file ? b->loc.file : "?",
                (int)b->loc.line, (int)b->loc.column);
        if (b->loc.proc) fprintf(stderr, " in `%s`", b->loc.proc);
        fputc('\n', stderr);
    }
    return n;
}

static int32_t wid_os_error_(int e) {
    switch (e) {
    case 0: return WID_OS_OK;
    case ENOENT: return WID_OS_NOT_FOUND;
    case EACCES:
    case EPERM: return WID_OS_PERMISSION_DENIED;
    case EEXIST: return WID_OS_EXISTS;
    case EISDIR: return WID_OS_IS_DIRECTORY;
    case ENOTDIR: return WID_OS_NOT_DIRECTORY;
    case ENOSPC: return WID_OS_NO_SPACE;
    case EINVAL:
    case ENAMETOOLONG: return WID_OS_INVALID;
    default: return WID_OS_IO;
    }
}

/* Copies `s` into a NUL-terminated string: `small` when it fits, else the heap. */
static char *wid_os_cstr_(wid_String s, char *small, size_t n) {
    char *out = (size_t)s.len < n ? small : malloc((size_t)s.len + 1);
    if (!out) return nullptr;
    if (s.len > 0) memcpy(out, s.data, (size_t)s.len);
    out[s.len] = '\0';
    return out;
}

wid_String *wid_os_args(void) { return wid_os_args_; }

int32_t wid_os_read_file(wid_String path, wid_Allocator a, uint8_t **data, wid_Int *len, wid_Location loc) {
    *data = nullptr;
    *len = 0;
    char small[512];
    char *p = wid_os_cstr_(path, small, sizeof small);
    if (!p) return WID_OS_IO;
    if (wid_os_is_dir(path)) {
        if (p != small) free(p);
        return WID_OS_IS_DIRECTORY;
    }
    errno = 0;
    FILE *f = fopen(p, "rb");
    int err = errno;
    if (p != small) free(p);
    if (!f) return wid_os_error_(err ? err : ENOENT);
    wid_Int first = 4096;
    if (fseek(f, 0, SEEK_END) == 0) {
        long size = ftell(f);
        if (size > 0) first = (wid_Int)size;
        rewind(f);
    }
    wid_Int cap = 0;
    wid_Int n = 0;
    uint8_t *buf = nullptr;
    for (;;) {
        if (n == cap) {
            /* Probe for the end before growing, so a file whose size was
             * known up front is read into exactly one block. */
            int c = cap ? fgetc(f) : 0;
            if (c == EOF) break;
            if (cap) ungetc(c, f);
            wid_Int grown = cap ? cap * 2 : first;
            buf = wid_resize(a, buf, cap, grown, 1, loc);
            cap = grown;
        }
        size_t got = fread(buf + n, 1, (size_t)(cap - n), f);
        n += (wid_Int)got;
        if (got == 0) break;
    }
    bool failed = ferror(f);
    fclose(f);
    if (failed) {
        if (buf) wid_free(a, buf, cap, loc);
        return WID_OS_IO;
    }
    if (n == 0 && buf) {
        wid_free(a, buf, cap, loc);
        buf = nullptr;
    } else if (n < cap) {
        buf = wid_resize(a, buf, cap, n, 1, loc);
    }
    *data = buf;
    *len = n;
    return WID_OS_OK;
}

int32_t wid_os_write_file(wid_String path, wid_String data, bool append) {
    char small[512];
    char *p = wid_os_cstr_(path, small, sizeof small);
    if (!p) return WID_OS_IO;
    errno = 0;
    FILE *f = fopen(p, append ? "ab" : "wb");
    int err = errno;
    if (p != small) free(p);
    if (!f) return wid_os_error_(err ? err : EACCES);
    size_t wrote = data.len > 0 ? fwrite(data.data, 1, (size_t)data.len, f) : 0;
    err = errno;
    bool failed = wrote != (size_t)data.len;
    if (fclose(f) != 0) failed = true;
    return failed ? wid_os_error_(err ? err : EIO) : WID_OS_OK;
}

int32_t wid_os_remove(wid_String path) {
    char small[512];
    char *p = wid_os_cstr_(path, small, sizeof small);
    if (!p) return WID_OS_IO;
    errno = 0;
    int r = remove(p);
    int err = errno;
    if (p != small) free(p);
    return r == 0 ? WID_OS_OK : wid_os_error_(err ? err : EIO);
}

int32_t wid_os_make_dir(wid_String path) {
    char small[512];
    char *p = wid_os_cstr_(path, small, sizeof small);
    if (!p) return WID_OS_IO;
    errno = 0;
    int r = mkdir(p, 0777);
    int err = errno;
    if (p != small) free(p);
    return r == 0 ? WID_OS_OK : wid_os_error_(err ? err : EIO);
}

bool wid_os_exists(wid_String path) {
    char small[512];
    char *p = wid_os_cstr_(path, small, sizeof small);
    if (!p) return false;
    struct stat st;
    bool found = stat(p, &st) == 0;
    if (p != small) free(p);
    return found;
}

bool wid_os_is_dir(wid_String path) {
    char small[512];
    char *p = wid_os_cstr_(path, small, sizeof small);
    if (!p) return false;
    struct stat st;
    bool dir = stat(p, &st) == 0 && S_ISDIR(st.st_mode);
    if (p != small) free(p);
    return dir;
}

wid_String wid_os_getenv(wid_String name, bool *found) {
    char small[256];
    char *p = wid_os_cstr_(name, small, sizeof small);
    const char *v = p ? getenv(p) : nullptr;
    if (p != small) free(p);
    *found = v != nullptr;
    return wid_string_from_cstring(v);
}

int32_t wid_os_read_line(wid_Allocator a, uint8_t **data, wid_Int *len, wid_Location loc) {
    *data = nullptr;
    *len = 0;
    fflush(stdout);
    wid_Int cap = 0;
    wid_Int n = 0;
    uint8_t *buf = nullptr;
    int c = EOF;
    while ((c = getchar()) != EOF && c != '\n') {
        if (n == cap) {
            wid_Int grown = cap ? cap * 2 : 128;
            buf = wid_resize(a, buf, cap, grown, 1, loc);
            cap = grown;
        }
        buf[n++] = (uint8_t)c;
    }
    if (c == EOF && n == 0) {
        if (buf) wid_free(a, buf, cap, loc);
        return ferror(stdin) ? WID_OS_IO : WID_OS_END;
    }
    if (n > 0 && buf[n - 1] == '\r') n--;
    *data = buf;
    *len = n;
    return WID_OS_OK;
}

void wid_os_write_stderr(wid_String s) {
    fflush(stdout);
    if (s.len > 0) fwrite(s.data, 1, (size_t)s.len, stderr);
}

void wid_os_exit(int32_t code) {
    wid_runtime_exit();
    exit(code);
}

wid_Int wid_fmt_float(double v, wid_Int precision, uint8_t style, uint8_t *buf, wid_Int cap) {
    char tmp[512];
    int n = 0;
    if (isnan(v) || isinf(v)) {
        n = snprintf(tmp, sizeof tmp, "%s", isnan(v) ? "NaN" : v < 0 ? "-Infinity" : "Infinity");
    } else if (precision < 0 && style != 'e') {
        wid_format_float_(v, false, tmp, sizeof tmp);
        n = (int)strlen(tmp);
    } else {
        int p = precision < 0 ? 16 : precision > 100 ? 100 : (int)precision;
        const char *f = style == 'e' ? "%.*e" : style == 'g' ? "%.*g" : "%.*f";
        n = snprintf(tmp, sizeof tmp, f, p, v);
    }
    if (n < 0) return 0;
    wid_Int len = n < cap ? n : cap;
    if (len > 0) memcpy(buf, tmp, (size_t)len);
    return len;
}

bool wid_parse_float(wid_String s, double *out) {
    char small[128];
    if (s.len <= 0 || s.len >= (wid_Int)sizeof small) return false;
    memcpy(small, s.data, (size_t)s.len);
    small[s.len] = '\0';
    if (small[0] == ' ' || small[0] == '\t' || small[0] == '\n') return false;
    char *end = nullptr;
    errno = 0;
    double v = strtod(small, &end);
    if (end != small + s.len || errno == ERANGE) return false;
    *out = v;
    return true;
}

#endif /* WID_RUNTIME_IMPLEMENTED */
#endif /* WID_RUNTIME_IMPLEMENTATION */
