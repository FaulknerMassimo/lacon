/* The Lacon native runtime. Generated programs include this header and link
 * against the runtime, which mirrors the interpreter's semantics
 * (crates/interp): every value is a tagged 16-byte `lc_v`; lists, strings,
 * maps and the rest are reference counted and copied on write, which gives
 * value semantics.
 *
 * Conventions: runtime functions borrow their `lc_v` arguments and return
 * owned values. `site` arguments are "file:line:col" strings for errors. */
#ifndef LACON_H
#define LACON_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

enum {
    T_UNIT, T_NONE, T_BOOL, T_INT, T_FLOAT,
    /* Heap values from here on. */
    T_STR, T_LIST, T_TUPLE, T_MAP, T_SET, T_HEAP, T_STRUCT, T_VARIANT, T_RANGE, T_ERR, T_FUNC,
    /* A function's slots, kept on the heap when lambdas capture them. */
    T_FRAME
};

typedef struct lc_obj { int64_t rc; } lc_obj;

typedef struct lc_v {
    uint32_t tag;
    union { int64_t i; double f; lc_obj *o; } u;
} lc_v;

typedef struct lc_str { int64_t rc; int64_t len; int64_t nchars; int64_t cap; char data[]; } lc_str;
/* Lists, tuples and heaps. */
typedef struct lc_vec { int64_t rc; int64_t len, cap; lc_v *items; } lc_vec;
typedef struct lc_entry { lc_v k, v; uint64_t h; } lc_entry;
/* Insertion-ordered maps and sets (sets leave `v` unit). */
typedef struct lc_map { int64_t rc; int64_t len, cap; lc_entry *e; int32_t *idx; int64_t icap; } lc_map;
/* Structs (tag 0) and enum variants. */
typedef struct lc_rec { int64_t rc; uint32_t ty, tag; int64_t n; lc_v f[]; } lc_rec;
typedef struct lc_range { int64_t rc; int64_t start, end, step; } lc_range;
typedef struct lc_err { int64_t rc; lc_v payload; } lc_err;

typedef struct lc_frame { int64_t rc; struct lc_frame *parent; int64_t n; lc_v slots[]; } lc_frame;
lc_frame *lc_frame_new(int64_t n, lc_frame *parent);
#define FRAME(v) ((lc_frame *)(v).u.o)

enum { FN_USER, FN_CLOSURE, FN_CTOR, FN_METHOD, FN_CONV };
struct lc_fn;
typedef lc_v (*lc_code)(struct lc_fn *self, int argc, lc_v *args, const char *site);
typedef struct lc_fn {
    int64_t rc;
    int kind, id, tag, nparams;
    lc_code code;
    int nenv;
    lc_v env[];
} lc_fn;

#define LC_UNIT ((lc_v){T_UNIT, {0}})
#define LC_NONE ((lc_v){T_NONE, {0}})
#define LC_TRUE ((lc_v){T_BOOL, {.i = 1}})
#define LC_FALSE ((lc_v){T_BOOL, {.i = 0}})
#define LC_IMMORTAL ((int64_t)1 << 60)

static inline __attribute__((always_inline)) lc_v lc_int(int64_t i) { lc_v v; v.tag = T_INT; v.u.i = i; return v; }
static inline __attribute__((always_inline)) lc_v lc_float(double f) { lc_v v; v.tag = T_FLOAT; v.u.f = f; return v; }
static inline __attribute__((always_inline)) lc_v lc_bool(bool b) { lc_v v; v.tag = T_BOOL; v.u.i = b; return v; }
static inline __attribute__((always_inline)) lc_v lc_obj_v(uint32_t tag, void *o) { lc_v v; v.tag = tag; v.u.o = (lc_obj *)o; return v; }

void lc_free(lc_v v);
static inline __attribute__((always_inline)) lc_v lc_retain(lc_v v) {
    if (v.tag >= T_STR) v.u.o->rc++;
    return v;
}
static inline __attribute__((always_inline)) void lc_release(lc_v v) {
    if (v.tag >= T_STR && --v.u.o->rc == 0) lc_free(v);
}
/* Stores an owned value into a slot, releasing what was there. */
static inline __attribute__((always_inline)) void lc_set(lc_v *slot, lc_v v) {
    lc_v old = *slot;
    *slot = v;
    lc_release(old);
}
/* Moves a value out of a slot, leaving unit. */
static inline __attribute__((always_inline)) lc_v lc_take(lc_v *slot) {
    lc_v v = *slot;
    *slot = LC_UNIT;
    return v;
}

#define STR(v) ((lc_str *)(v).u.o)
#define VEC(v) ((lc_vec *)(v).u.o)
#define MAP(v) ((lc_map *)(v).u.o)
#define REC(v) ((lc_rec *)(v).u.o)
#define RANGE(v) ((lc_range *)(v).u.o)
#define ERRV(v) ((lc_err *)(v).u.o)
#define FN(v) ((lc_fn *)(v).u.o)

/* ----- program description, filled in by the generated code ----- */

typedef struct { const char *name; int nfields; const int *field_ids; const char *const *field_names; } lc_struct_info;
typedef struct { const char *name; int nfields; } lc_variant_info;
typedef struct { const char *name; int nvariants; const lc_variant_info *variants; } lc_enum_info;
typedef struct {
    const lc_struct_info *structs;
    const lc_enum_info *enums;
    const char *const *fn_names;
    const char *const *field_names;
} lc_program;
extern const lc_program *lc_prog;

/* Declared types, for the checks the interpreter makes at function, struct
 * and return boundaries. */
enum { TY_ANY, TY_INT, TY_FLOAT, TY_BOOL, TY_STR, TY_UNIT, TY_LIST, TY_MAP, TY_SET, TY_HEAP, TY_TUPLE, TY_OPT, TY_RES, TY_FN, TY_STRUCT, TY_ENUM };
typedef struct lc_ty {
    int kind;
    const char *name;
    int64_t lo, hi;
    const struct lc_ty *a, *b;
    int n;
    const struct lc_ty *const *elems;
    int id;
} lc_ty;

/* ----- strings and buffers ----- */

typedef struct { char *p; int64_t len, cap; } lc_buf;
void lc_buf_put(lc_buf *b, const char *s, int64_t n);
void lc_buf_puts(lc_buf *b, const char *s);
void lc_buf_putc(lc_buf *b, char c);
void lc_buf_display(lc_buf *b, lc_v v);
void lc_buf_repr(lc_buf *b, lc_v v);
lc_v lc_buf_finish(lc_buf *b);

lc_v lc_str_new(const char *s, int64_t len);
lc_v lc_cstr(const char *s);
lc_v lc_str_lit(const char *s, int64_t len);
lc_v lc_str_cat(lc_str *a, lc_str *b);
int lc_fmt_int(char *buf, int64_t v);
lc_v lc_display(lc_v v);
lc_v lc_repr(lc_v v);
int64_t lc_str_nchars(lc_str *s);
const char *lc_kind(lc_v v, char *buf);

/* ----- collections ----- */

lc_v lc_list_new(int64_t cap);
lc_v lc_tuple_new(int64_t cap);
void lc_vec_push(lc_v vec, lc_v item);
lc_v lc_list_of(int n, lc_v *items);
lc_v lc_tuple_of(int n, lc_v *items);
lc_v lc_map_new(void);
lc_v lc_set_new(void);
lc_v lc_heap_new(void);
void lc_map_put(lc_v map, lc_v k, lc_v v);
bool lc_set_add(lc_v set, lc_v k);
lc_entry *lc_map_find(lc_map *m, lc_v k);
lc_v lc_rec_new(uint32_t tag, uint32_t ty, uint32_t vtag, int64_t n, lc_v *fields);
lc_v lc_range_new(int64_t start, int64_t end, int64_t step);
lc_v lc_err_new(lc_v payload);
lc_v lc_closure_new(lc_code code, int nparams, int nenv, lc_v *env);
lc_v lc_fnval_new(int kind, int id, int tag, lc_code code);
int64_t lc_range_len(lc_range *r);

/* ----- values ----- */

int lc_eq(lc_v a, lc_v b);            /* 1, 0, or -1 when incomparable */
int lc_cmp(lc_v a, lc_v b, bool *ok); /* -1, 0, 1 */
bool lc_key_eq(lc_v a, lc_v b);
uint64_t lc_hash(lc_v v);
void lc_fmt_float(lc_buf *b, double f);

/* ----- errors, calls, output ----- */

_Noreturn void lc_panic(const char *code, const char *site, const char *fmt, ...);
_Noreturn void lc_exit(int code);
void lc_out(const char *s, int64_t n);
void lc_flush(void);

extern const char *lc_callsite;
extern int64_t lc_depth;
void lc_enter_slow(int fn_id);
typedef struct { int fn; const char *site; } lc_call_rec;
extern lc_call_rec *lc_frames;
extern int64_t lc_frames_cap;
/* The frame array never holds more than the depth limit, so one bound check
 * covers both. */
static inline __attribute__((always_inline)) void lc_enter(int fn_id) {
    int64_t d = lc_depth;
    if (__builtin_expect(d < lc_frames_cap, 1)) {
        lc_frames[d] = (lc_call_rec){fn_id, lc_callsite};
        lc_depth = d + 1;
    } else {
        lc_enter_slow(fn_id);
    }
}
static inline __attribute__((always_inline)) void lc_leave(void) { lc_depth--; }
/* A lambda call: counts toward the depth limit but shows in no trace. */
void lc_enter_lambda(const char *site);

/* `?` inside a lambda leaves the enclosing named function: the lambda sets
 * this and returns, and every caller up to that function passes it on. */
extern int lc_unwinding;
extern lc_v lc_unwind_val;

int lc_main(int argc, char **argv, const lc_program *prog, void (*init)(void), lc_v (*main_fn)(lc_v *));

/* ----- operators ----- */

enum { OP_ADD, OP_SUB, OP_MUL, OP_DIV, OP_REM, OP_POW, OP_BITAND, OP_BITOR, OP_BITXOR, OP_SHL, OP_SHR };
enum { CMP_EQ, CMP_NE, CMP_LT, CMP_LE, CMP_GT, CMP_GE, CMP_IN, CMP_NOTIN };
enum { UN_NEG, UN_NOT, UN_BANG };

lc_v lc_binop(int op, lc_v a, lc_v b, const char *site);
/* `lc_binop` that consumes `a`, appending in place when nothing else holds it. */
lc_v lc_binop_own(int op, lc_v a, lc_v b, const char *site);
bool lc_pat_range(lc_v v, bool has_lo, lc_v lo, bool has_hi, lc_v hi, bool inclusive);
lc_v lc_list_from(lc_v v, int64_t start);
lc_v lc_unop(int op, lc_v a, const char *site);
bool lc_compare(int op, lc_v a, lc_v b, const char *site);
bool lc_truth(lc_v c, const char *site);
bool lc_contains(lc_v container, lc_v x, const char *site);
lc_v lc_index(lc_v o, lc_v i, const char *site);
lc_v lc_slice(lc_v o, bool has_start, int64_t start, bool has_end, int64_t end, bool inclusive, const char *site);
lc_v lc_make_range(bool has_start, lc_v start, bool has_end, lc_v end, bool inclusive, const char *site);
int64_t lc_int_of(lc_v v, const char *site);

/* `o.name`: a struct field or tuple element (sets *found), else nothing. */
lc_v lc_field(lc_v o, int field_id, const char *name, bool *found, const char *site);
/* `o.name` that is no field: a method without arguments, or an error. */
lc_v lc_field_fallback(lc_v o, const char *name, int method, const char *site);
lc_v lc_coerce_ret(lc_v v, const lc_ty *t, const char *fn, const char *site);
lc_v lc_coerce_arg(lc_v v, const lc_ty *t, const char *param, const char *fn, const char *site);
lc_v lc_coerce_field(lc_v v, const lc_ty *t, const char *field, const char *owner, bool variant, const char *site);
_Noreturn void lc_no_arm(lc_v v, const char *site);
_Noreturn void lc_bad_unpack(lc_v v, int n, bool in_for, const char *site);
void lc_check_ignored(lc_v v, const char *site);
lc_v lc_try_none(bool fn_optional);
lc_v lc_short(lc_v v);
bool lc_assert_eq(lc_v a, lc_v b, lc_v msg, bool has_msg, const char *site);
_Noreturn void lc_assert_fail(lc_v msg, bool has_msg, const char *site);

enum { CONV_INT, CONV_FLOAT, CONV_STR, CONV_BOOL };
/* `int`, `u8`, ..., `f64`, `str`, `bool`, in the order of
 * `lacon_interp::resolve::INT_TYPES` then `f64`, `f32`, `str`, `bool`. */
typedef struct { const char *name; int to; int64_t lo, hi; } lc_conv_info;
extern const lc_conv_info lc_convs[];
lc_v lc_convert(lc_v v, int to, const char *name, int64_t lo, int64_t hi, const char *site);
/* `s.parse()` where the checker knows the type wanted (`n int = s.parse()?`):
 * a number of the other kind, or one out of range, is an error. */
lc_v lc_parse_as(lc_v v, int to, const char *name, int64_t lo, int64_t hi, const char *site);

bool lc_ty_matches(lc_v v, const lc_ty *t);
/* Checks an owned value against a declared type; on mismatch returns false
 * and describes the value in `got`. */
bool lc_coerce(lc_v *v, const lc_ty *t, char *got, size_t gotlen);
const char *lc_ty_name(const lc_ty *t, char *buf, size_t len);

/* ----- places ----- */

enum { VIV_NO, VIV_INSERT, VIV_ZERO_OF, VIV_METHOD, VIV_MAP };
lc_v *lc_place_field(lc_v *p, int field_id, const char *name, const char *site);
lc_v *lc_place_index(lc_v *p, lc_v key, int viv, lc_v zero_of, int method, const char *site);
void lc_make_unique(lc_v *p);

/* ----- iteration ----- */

typedef struct {
    lc_v src;
    int kind;
    int64_t i, n;
    int64_t start, step;
    lc_v sorted;
} lc_iter;
enum { IT_RANGE, IT_VEC, IT_KEYS, IT_PAIRS, IT_STR };
void lc_iter_init(lc_iter *it, lc_v v, bool keys_only, const char *site);
bool lc_iter_next(lc_iter *it, lc_v *out);
void lc_iter_done(lc_iter *it);
/* Elements of any iterable as a new list, for patterns and methods. */
lc_v lc_items(lc_v v, const char *site);

/* ----- calls ----- */

lc_v lc_call(lc_v f, int argc, lc_v *args, const char *site);

/* ----- formatting ----- */

typedef struct {
    int fill; /* -1 when not written */
    char align, sign;
    bool alt, zero;
    int64_t width;
    char grouping;
    int64_t precision; /* -1 when absent */
    char kind;
} lc_fmt;
void lc_buf_format(lc_buf *b, lc_v v, const lc_fmt *spec, const char *site);

/* ----- builtins ----- */

void lc_print(int argc, lc_v *args, bool err, const char *site);
lc_v lc_range_fn(int argc, lc_v *args, const char *site);
lc_v lc_minmax(bool max, int argc, lc_v *args, const char *site);
lc_v lc_collect(int kind, int argc, lc_v *args, const char *site);
_Noreturn void lc_panic_fn(lc_v msg, const char *site);
lc_v lc_fs(int op, int argc, lc_v *args, const char *site);
lc_v lc_io(int op, int argc, lc_v *args, const char *site);
lc_v lc_os(int op, int argc, lc_v *args, const char *site);
enum { FS_READ, FS_WRITE, FS_APPEND, FS_EXISTS, FS_LINES, FS_REMOVE };
enum { IO_READ, IO_LINES, IO_READ_LINE, IO_WRITE };
enum { OS_ARGS, OS_ENV, OS_EXIT, TIME_NOW };

/* Methods, in the order of `lacon_interp::builtins::METHODS`. */
enum {
    M_str, M_to_string, M_unwrap, M_expect, M_is_none, M_is_some, M_is_err, M_is_ok, M_clone, M_iter, M_into_iter, M_collect,
    M_to_owned, M_copied, M_cloned, M_as_str, M_abs, M_pow, M_min, M_max, M_clamp, M_sign, M_chr, M_sqrt, M_cbrt, M_floor,
    M_ceil, M_round, M_trunc, M_fract, M_exp, M_ln, M_log, M_log2, M_log10, M_sin, M_cos, M_tan, M_asin, M_acos, M_atan,
    M_atan2, M_hypot, M_is_nan, M_is_finite, M_len, M_is_empty, M_chars, M_bytes, M_lines, M_split, M_split_once, M_words,
    M_trim, M_trim_start, M_trim_end, M_starts_with, M_ends_with, M_contains, M_contains_key, M_find, M_rfind, M_replace,
    M_upper, M_lower, M_repeat, M_parse, M_rev, M_count, M_is_digit, M_is_alpha, M_is_alnum, M_is_space, M_is_upper,
    M_is_lower, M_ord, M_join, M_strip_prefix, M_strip_suffix, M_pad_left, M_pad_right, M_capitalize, M_first, M_last, M_get,
    M_index, M_map, M_filter, M_position, M_any, M_all, M_sum, M_product, M_min_by, M_max_by, M_sort, M_sort_by, M_unique,
    M_enumerate, M_zip, M_flat_map, M_flatten, M_take, M_skip, M_take_while, M_skip_while, M_chunks, M_windows, M_group_by,
    M_partition, M_fold, M_reduce, M_each, M_to_list, M_to_set, M_to_map, M_step_by, M_keys, M_values, M_items, M_union,
    M_intersection, M_difference, M_is_subset, M_peek, M_push, M_pop, M_insert, M_remove, M_clear, M_extend, M_add, M_swap,
    M_truncate, M_retain,
    M_COUNT
};
extern const char *const lc_method_names[];
bool lc_is_mutator(int m);
lc_v lc_method(int m, lc_v recv, int argc, lc_v *args, const char *site);
lc_v lc_mutate(int m, lc_v *place, int argc, lc_v *args, const char *site);

/* ----- fast paths ----- */

static inline __attribute__((always_inline)) lc_v lc_add(lc_v a, lc_v b, const char *site) {
    int64_t r;
    if (a.tag == T_INT && b.tag == T_INT && !__builtin_add_overflow(a.u.i, b.u.i, &r)) return lc_int(r);
    return lc_binop(OP_ADD, a, b, site);
}
static inline __attribute__((always_inline)) lc_v lc_sub(lc_v a, lc_v b, const char *site) {
    int64_t r;
    if (a.tag == T_INT && b.tag == T_INT && !__builtin_sub_overflow(a.u.i, b.u.i, &r)) return lc_int(r);
    return lc_binop(OP_SUB, a, b, site);
}
static inline __attribute__((always_inline)) bool lc_cmp_fast(int op, lc_v a, lc_v b, const char *site) {
    if (a.tag == T_INT && b.tag == T_INT) {
        switch (op) {
        case CMP_EQ: return a.u.i == b.u.i;
        case CMP_NE: return a.u.i != b.u.i;
        case CMP_LT: return a.u.i < b.u.i;
        case CMP_LE: return a.u.i <= b.u.i;
        case CMP_GT: return a.u.i > b.u.i;
        case CMP_GE: return a.u.i >= b.u.i;
        }
    }
    return lc_compare(op, a, b, site);
}
/* `lc_binop_own` with ints done inline. Consumes both operands. */
static inline __attribute__((always_inline)) lc_v lc_arith_own(int op, lc_v a, lc_v b, const char *site) {
    if (a.tag == T_INT && b.tag == T_INT) {
        int64_t r;
        switch (op) {
        case OP_ADD:
            if (!__builtin_add_overflow(a.u.i, b.u.i, &r)) return lc_int(r);
            break;
        case OP_SUB:
            if (!__builtin_sub_overflow(a.u.i, b.u.i, &r)) return lc_int(r);
            break;
        case OP_MUL:
            if (!__builtin_mul_overflow(a.u.i, b.u.i, &r)) return lc_int(r);
            break;
        }
    }
    lc_v r = lc_binop_own(op, a, b, site);
    lc_release(b);
    return r;
}
/* The next element of a list being looped over, else the general case. */
static inline __attribute__((always_inline)) bool lc_iter_next_fast(lc_iter *it, lc_v *out) {
    if (it->kind == IT_VEC) {
        if (it->i >= it->n) return false;
        *out = lc_retain(VEC(it->src)->items[it->i++]);
        return true;
    }
    return lc_iter_next(it, out);
}
/* `xs[i]` with an unboxed index; borrows `o`. */
static inline __attribute__((always_inline)) lc_v lc_index_int(lc_v o, int64_t i, const char *site) {
    if (o.tag == T_LIST || o.tag == T_TUPLE) {
        lc_vec *v = VEC(o);
        int64_t k = i < 0 ? v->len + i : i;
        if (k >= 0 && k < v->len) return lc_retain(v->items[k]);
    }
    return lc_index(o, lc_int(i), site);
}
/* `o.len`, which is a struct's field if it has one; borrows `o`. */
int64_t lc_len_slow(lc_v o, int field_id, const char *site);
static inline __attribute__((always_inline)) int64_t lc_len(lc_v o, int field_id, const char *site) {
    if (o.tag == T_LIST) return VEC(o)->len;
    if (o.tag == T_STR) return STR(o)->nchars;
    return lc_len_slow(o, field_id, site);
}
/* `xs[i]` on a list with an int index in range, else the general case. */
static inline __attribute__((always_inline)) lc_v lc_index_fast(lc_v o, lc_v i, const char *site) {
    if ((o.tag == T_LIST || o.tag == T_TUPLE) && i.tag == T_INT) {
        lc_vec *v = VEC(o);
        int64_t k = i.u.i < 0 ? v->len + i.u.i : i.u.i;
        if (k >= 0 && k < v->len) return lc_retain(v->items[k]);
    }
    return lc_index(o, i, site);
}
static inline __attribute__((always_inline)) lc_v *lc_place_index_fast(lc_v *p, lc_v key, int viv, lc_v zero, int method, const char *site) {
    if (p->tag == T_LIST && key.tag == T_INT && p->u.o->rc == 1) {
        lc_vec *v = VEC(*p);
        int64_t k = key.u.i < 0 ? v->len + key.u.i : key.u.i;
        if (k >= 0 && k < v->len) return &v->items[k];
    }
    return lc_place_index(p, key, viv, zero, method, site);
}
static inline __attribute__((always_inline)) bool lc_test(lc_v c, const char *site) {
    if (c.tag == T_BOOL) return c.u.i != 0;
    return lc_truth(c, site);
}

/* ----- unboxed ints and bools -----
 *
 * Where the checker proves a value is an int or a bool, generated code holds
 * it as an `int64_t` or `bool`. These are the runtime's int operators on
 * such values, with the same errors. */

/* A value the checker typed as an int or a bool turned out not to be one: a
 * checker bug, never a program's. */
_Noreturn void lc_unbox_fail(lc_v v, const char *want, const char *site);
static inline __attribute__((always_inline)) int64_t lc_unbox_int(lc_v v, const char *site) {
    if (__builtin_expect(v.tag == T_INT, 1)) return v.u.i;
    lc_unbox_fail(v, "int", site);
}
static inline __attribute__((always_inline)) bool lc_unbox_bool(lc_v v, const char *site) {
    if (__builtin_expect(v.tag == T_BOOL, 1)) return v.u.i != 0;
    lc_unbox_fail(v, "bool", site);
}
/* A float the checker proved is one. */
static inline __attribute__((always_inline)) double lc_unbox_float(lc_v v, const char *site) {
    if (__builtin_expect(v.tag == T_FLOAT, 1)) return v.u.f;
    lc_unbox_fail(v, "f64", site);
}
/* A number typed f64, which may still hold an int (`if c: 1 else: 2.5`):
 * as the runtime's float operators see it. */
static inline __attribute__((always_inline)) double lc_num(lc_v v, const char *site) {
    if (v.tag == T_FLOAT) return v.u.f;
    if (__builtin_expect(v.tag == T_INT, 1)) return (double)v.u.i;
    lc_unbox_fail(v, "number", site);
}
/* Comparing unboxed floats; ordering NaN is the runtime's error. */
static inline __attribute__((always_inline)) bool lc_fcmp(int op, double a, double b, const char *site) {
    if (__builtin_expect(a != a || b != b, 0)) return lc_compare(op, lc_float(a), lc_float(b), site);
    switch (op) {
    case CMP_EQ: return a == b;
    case CMP_NE: return a != b;
    case CMP_LT: return a < b;
    case CMP_LE: return a <= b;
    case CMP_GT: return a > b;
    default: return a >= b;
    }
}
static inline __attribute__((always_inline)) int64_t lc_iadd(int64_t a, int64_t b, const char *site) {
    int64_t r;
    if (__builtin_add_overflow(a, b, &r)) lc_panic("E0405", site, "integer overflow in `%s`", "+");
    return r;
}
static inline __attribute__((always_inline)) int64_t lc_isub(int64_t a, int64_t b, const char *site) {
    int64_t r;
    if (__builtin_sub_overflow(a, b, &r)) lc_panic("E0405", site, "integer overflow in `%s`", "-");
    return r;
}
static inline __attribute__((always_inline)) int64_t lc_imul(int64_t a, int64_t b, const char *site) {
    int64_t r;
    if (__builtin_mul_overflow(a, b, &r)) lc_panic("E0405", site, "integer overflow in `%s`", "*");
    return r;
}
static inline __attribute__((always_inline)) int64_t lc_idiv(int64_t a, int64_t b, const char *site) {
    if (b == 0) lc_panic("E0404", site, "division by zero");
    if (a == INT64_MIN && b == -1) lc_panic("E0405", site, "integer overflow in `%s`", "/");
    return a / b;
}
static inline __attribute__((always_inline)) int64_t lc_irem(int64_t a, int64_t b, const char *site) {
    if (b == 0) lc_panic("E0404", site, "division by zero");
    if (a == INT64_MIN && b == -1) lc_panic("E0405", site, "integer overflow in `%s`", "%");
    return a % b;
}
static inline __attribute__((always_inline)) int64_t lc_ishl(int64_t a, int64_t b, const char *site) {
    if (b < 0 || b >= 64) lc_panic("E0405", site, "integer overflow in `%s`", "<<");
    return (int64_t)((uint64_t)a << b);
}
static inline __attribute__((always_inline)) int64_t lc_ishr(int64_t a, int64_t b, const char *site) {
    if (b < 0 || b >= 64) lc_panic("E0405", site, "integer overflow in `%s`", ">>");
    return a >> b;
}
static inline __attribute__((always_inline)) int64_t lc_ineg(int64_t a, const char *site) {
    if (a == INT64_MIN) lc_panic("E0405", site, "integer overflow");
    return -a;
}

#endif
