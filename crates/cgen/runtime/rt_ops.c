/* Operators, indexing, conversions, declared types, places, iteration,
 * calls, format specs and builtin functions. Mirrors crates/interp/src/eval.rs,
 * builtins.rs and crates/syntax/src/fmtspec.rs. */
#include "lacon.h"

#include <errno.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

typedef struct { lc_v key, val; } lc_kv;
bool lc_sort_kv(lc_kv *xs, int64_t n, char *a, char *b);
lc_v lc_heap_sorted(lc_v h);
lc_v lc_map_remove(lc_v map, lc_v k, bool *found);
void lc_fmt_float_display(lc_buf *b, double f);
extern int lc_argc;
extern char **lc_argv;

const lc_conv_info lc_convs[] = {
    {"int", CONV_INT, INT64_MIN, INT64_MAX}, {"i64", CONV_INT, INT64_MIN, INT64_MAX}, {"usize", CONV_INT, 0, INT64_MAX},
    {"isize", CONV_INT, INT64_MIN, INT64_MAX}, {"i32", CONV_INT, INT32_MIN, INT32_MAX}, {"i16", CONV_INT, INT16_MIN, INT16_MAX},
    {"i8", CONV_INT, INT8_MIN, INT8_MAX}, {"u64", CONV_INT, 0, INT64_MAX}, {"u32", CONV_INT, 0, UINT32_MAX},
    {"u16", CONV_INT, 0, UINT16_MAX}, {"u8", CONV_INT, 0, UINT8_MAX}, {"f64", CONV_FLOAT, 0, 0}, {"f32", CONV_FLOAT, 0, 0},
    {"str", CONV_STR, 0, 0}, {"bool", CONV_BOOL, 0, 0},
};

static const char *disp(lc_v v, lc_buf *b) {
    b->len = 0;
    lc_buf_display(b, v);
    return b->p ? b->p : "";
}

/* A short form of a value for messages: its repr, cut to 60 characters. */
static const char *short_repr(lc_v v, lc_buf *b) {
    b->len = 0;
    lc_buf_repr(b, v);
    if (!b->p) return "";
    int64_t chars = 0, cut = -1;
    for (int64_t i = 0; i < b->len; i++) {
        if (((unsigned char)b->p[i] & 0xC0) != 0x80) {
            if (chars == 57) cut = i;
            chars++;
        }
    }
    if (chars > 60) {
        b->len = cut;
        b->p[cut] = 0;
        lc_buf_puts(b, "...");
    }
    return b->p;
}

lc_v lc_short(lc_v v) {
    lc_buf b = {0};
    short_repr(v, &b);
    return lc_buf_finish(&b);
}

static const char *op_symbol(int op) {
    static const char *s[] = {"+", "-", "*", "/", "%", "**", "&", "|", "^", "<<", ">>"};
    return s[op];
}


static lc_v str_repeat(lc_str *s, int64_t n) {
    lc_buf b = {0};
    for (int64_t i = 0; i < n; i++) lc_buf_put(&b, s->data, s->len);
    if (!b.p) return lc_str_new("", 0);
    return lc_buf_finish(&b);
}

static bool checked_pow(int64_t x, int64_t y, int64_t *out) {
    if (y < 0 || y > UINT32_MAX) return false;
    int64_t r = 1, b = x;
    uint64_t e = (uint64_t)y;
    while (e) {
        if (e & 1 && __builtin_mul_overflow(r, b, &r)) return false;
        e >>= 1;
        if (e && __builtin_mul_overflow(b, b, &b)) return false;
    }
    *out = r;
    return true;
}

static double as_f64(lc_v v) { return v.tag == T_INT ? (double)v.u.i : v.u.f; }

static lc_v set_op(int op, lc_v a, lc_v b) {
    lc_map *x = MAP(a), *y = MAP(b);
    lc_v out = lc_set_new();
    switch (op) {
    case OP_BITOR:
        for (int64_t i = 0; i < x->len; i++) lc_set_add(out, lc_retain(x->e[i].k));
        for (int64_t i = 0; i < y->len; i++) lc_set_add(out, lc_retain(y->e[i].k));
        break;
    case OP_BITAND:
        for (int64_t i = 0; i < x->len; i++)
            if (lc_map_find(y, x->e[i].k)) lc_set_add(out, lc_retain(x->e[i].k));
        break;
    case OP_SUB:
        for (int64_t i = 0; i < x->len; i++)
            if (!lc_map_find(y, x->e[i].k)) lc_set_add(out, lc_retain(x->e[i].k));
        break;
    default:
        for (int64_t i = 0; i < x->len; i++)
            if (!lc_map_find(y, x->e[i].k)) lc_set_add(out, lc_retain(x->e[i].k));
        for (int64_t i = 0; i < y->len; i++)
            if (!lc_map_find(x, y->e[i].k)) lc_set_add(out, lc_retain(y->e[i].k));
    }
    return out;
}

lc_v lc_binop(int op, lc_v a, lc_v b, const char *site) {
    char k1[64], k2[64];
    lc_buf t = {0};
    if (a.tag == T_ERR || b.tag == T_ERR)
        lc_panic("E0406", site, "operating on an error (%s); add `?`", disp(a.tag == T_ERR ? a : b, &t));
    if (a.tag == T_INT && b.tag == T_INT) {
        int64_t x = a.u.i, y = b.u.i, r;
        switch (op) {
        case OP_ADD:
            if (__builtin_add_overflow(x, y, &r)) break;
            return lc_int(r);
        case OP_SUB:
            if (__builtin_sub_overflow(x, y, &r)) break;
            return lc_int(r);
        case OP_MUL:
            if (__builtin_mul_overflow(x, y, &r)) break;
            return lc_int(r);
        case OP_DIV:
        case OP_REM:
            if (y == 0) lc_panic("E0404", site, "division by zero");
            if (x == INT64_MIN && y == -1) break;
            return lc_int(op == OP_DIV ? x / y : x % y);
        case OP_POW:
            if (y < 0) return lc_float(pow((double)x, (double)y));
            if (!checked_pow(x, y, &r)) break;
            return lc_int(r);
        case OP_BITAND: return lc_int(x & y);
        case OP_BITOR: return lc_int(x | y);
        case OP_BITXOR: return lc_int(x ^ y);
        case OP_SHL:
            if (y < 0 || y >= 64) break;
            return lc_int((int64_t)((uint64_t)x << y));
        case OP_SHR:
            if (y < 0 || y >= 64) break;
            return lc_int(x >> y);
        }
        lc_panic("E0405", site, "integer overflow in `%s`", op_symbol(op));
    }
    if (a.tag == T_BOOL && b.tag == T_BOOL && (op == OP_BITAND || op == OP_BITOR || op == OP_BITXOR)) {
        int64_t x = a.u.i, y = b.u.i;
        return lc_bool(op == OP_BITAND ? (x & y) : op == OP_BITOR ? (x | y) : (x ^ y));
    }
    if ((a.tag == T_INT || a.tag == T_FLOAT) && (b.tag == T_INT || b.tag == T_FLOAT)) {
        double x = as_f64(a), y = as_f64(b);
        switch (op) {
        case OP_ADD: return lc_float(x + y);
        case OP_SUB: return lc_float(x - y);
        case OP_MUL: return lc_float(x * y);
        case OP_DIV: return lc_float(x / y);
        case OP_REM: return lc_float(fmod(x, y));
        case OP_POW: return lc_float(pow(x, y));
        }
        lc_panic("E0301", site, "`%s` needs ints, got f64", op_symbol(op));
    }
    if (op == OP_ADD && a.tag == T_STR && b.tag == T_STR) return lc_str_cat(STR(a), STR(b));
    if (op == OP_MUL && a.tag == T_STR && b.tag == T_INT) return str_repeat(STR(a), b.u.i > 0 ? b.u.i : 0);
    if (op == OP_MUL && a.tag == T_INT && b.tag == T_STR) return str_repeat(STR(b), a.u.i > 0 ? a.u.i : 0);
    if (op == OP_ADD && a.tag == T_LIST && b.tag == T_LIST) {
        lc_vec *x = VEC(a), *y = VEC(b);
        lc_v out = lc_list_new(x->len + y->len);
        for (int64_t i = 0; i < x->len; i++) lc_vec_push(out, lc_retain(lc_vget(x, i)));
        for (int64_t i = 0; i < y->len; i++) lc_vec_push(out, lc_retain(lc_vget(y, i)));
        return out;
    }
    if (op == OP_ADD && a.tag == T_LIST && b.tag == T_RANGE) {
        lc_vec *x = VEC(a);
        lc_range *r = RANGE(b);
        int64_t n = lc_range_len(r);
        lc_v out = lc_list_new(x->len + n);
        for (int64_t i = 0; i < x->len; i++) lc_vec_push(out, lc_retain(lc_vget(x, i)));
        for (int64_t i = 0; i < n; i++) lc_vec_push(out, lc_int(r->start + r->step * i));
        return out;
    }
    if (op == OP_MUL && ((a.tag == T_LIST && b.tag == T_INT) || (a.tag == T_INT && b.tag == T_LIST))) {
        lc_vec *x = a.tag == T_LIST ? VEC(a) : VEC(b);
        int64_t n = a.tag == T_INT ? a.u.i : b.u.i;
        if (n < 0) n = 0;
        if (x->kind != K_BOXED) {
            /* Packed: copied a block at a time. */
            lc_v out = lc_vec_alloc(T_LIST, x->kind, x->len * n);
            size_t sz = (size_t)x->len * lc_ksize(x->kind);
            if (sz)
                for (int64_t k = 0; k < n; k++) memcpy((char *)VEC(out)->data + sz * k, x->data, sz);
            VEC(out)->len = x->len * n;
            return out;
        }
        lc_v out = lc_list_new(x->len * n);
        for (int64_t k = 0; k < n; k++)
            for (int64_t i = 0; i < x->len; i++) lc_vec_push(out, lc_retain(x->boxed[i]));
        return out;
    }
    if (a.tag == T_SET && b.tag == T_SET && (op == OP_BITOR || op == OP_BITAND || op == OP_SUB || op == OP_BITXOR)) return set_op(op, a, b);
    if (op == OP_BITOR && a.tag == T_MAP && b.tag == T_MAP) {
        lc_map *x = MAP(a), *y = MAP(b);
        lc_v out = lc_map_new();
        for (int64_t i = 0; i < x->len; i++) lc_map_put(out, lc_retain(x->e[i].k), lc_retain(x->e[i].v));
        for (int64_t i = 0; i < y->len; i++) lc_map_put(out, lc_retain(y->e[i].k), lc_retain(y->e[i].v));
        return out;
    }
    if (a.tag == T_NONE || b.tag == T_NONE)
        lc_panic("E0407", site, "`%s` on none; check for `none` first or use `x ?? default`", op_symbol(op));
    if (op == OP_REM && a.tag == T_STR) lc_panic("E0301", site, "no `%%` formatting; every string interpolates: \"{n} items, {x:.2}\"");
    if (op == OP_ADD && (a.tag == T_STR || b.tag == T_STR))
        lc_panic("E0301", site, "cannot add str and %s; interpolate instead: \"{a}{b}\"", lc_kind(a.tag == T_STR ? b : a, k1));
    lc_panic("E0301", site, "cannot apply `%s` to %s and %s", op_symbol(op), lc_kind(a, k1), lc_kind(b, k2));
}

lc_v lc_str_append(lc_v a, const char *s, int64_t n);

lc_v lc_binop_own(int op, lc_v a, lc_v b, const char *site) {
    if (op == OP_ADD && a.tag >= T_STR && a.u.o->rc == 1) {
        if (a.tag == T_STR && b.tag == T_STR) return lc_str_append(a, STR(b)->data, STR(b)->len);
        if (a.tag == T_LIST && b.tag == T_LIST) {
            lc_vec *y = VEC(b);
            int64_t n = y->len;
            for (int64_t i = 0; i < n; i++) lc_vec_push(a, lc_retain(lc_vget(y, i)));
            return a;
        }
    }
    lc_v r = lc_binop(op, a, b, site);
    lc_release(a);
    return r;
}

bool lc_pat_range(lc_v v, bool has_lo, lc_v lo, bool has_hi, lc_v hi, bool inclusive) {
    if (v.tag == T_NONE || v.tag == T_ERR) return false;
    bool ok;
    if (has_lo) {
        int c = lc_cmp(v, lo, &ok);
        if (!ok || c < 0) return false;
    }
    if (has_hi) {
        int c = lc_cmp(v, hi, &ok);
        if (!ok || (inclusive ? c > 0 : c >= 0)) return false;
    }
    return true;
}

lc_v lc_list_from(lc_v v, int64_t start) {
    return lc_vec_slice(v, T_LIST, start, VEC(v)->len);
}

lc_v lc_unop(int op, lc_v v, const char *site) {
    char k[64];
    lc_buf t = {0};
    if (op == UN_NEG && v.tag == T_INT) {
        if (v.u.i == INT64_MIN) lc_panic("E0405", site, "integer overflow");
        return lc_int(-v.u.i);
    }
    if (op == UN_NEG && v.tag == T_FLOAT) return lc_float(-v.u.f);
    if (op != UN_NEG && v.tag == T_BOOL) return lc_bool(!v.u.i);
    if (op == UN_BANG && v.tag == T_INT) return lc_int(~v.u.i);
    if (op == UN_NEG && v.tag == T_TUPLE) {
        lc_vec *x = VEC(v);
        lc_v out = lc_tuple_new(x->len);
        for (int64_t i = 0; i < x->len; i++) {
            lc_v e = x->boxed[i];
            if (e.tag == T_INT) {
                lc_vec_push(out, lc_int(-e.u.i));
            } else if (e.tag == T_FLOAT) {
                lc_vec_push(out, lc_float(-e.u.f));
            } else {
                lc_panic("E0301", site, "cannot negate %s", lc_kind(v, k));
            }
        }
        return out;
    }
    if (v.tag == T_ERR) lc_panic("E0406", site, "operating on an error (%s); add `?`", disp(v, &t));
    lc_panic("E0301", site, "cannot %s %s", op == UN_NEG ? "negate" : "apply `not` to", lc_kind(v, k));
}

bool lc_truth(lc_v c, const char *site) {
    char k[64];
    lc_buf t = {0};
    if (c.tag == T_BOOL) return c.u.i != 0;
    if (c.tag == T_ERR) lc_panic("E0406", site, "condition is an error (%s); add `?`", disp(c, &t));
    const char *hint;
    switch (c.tag) {
    case T_LIST:
    case T_MAP:
    case T_SET:
    case T_HEAP:
    case T_STR: hint = "; test emptiness with `x.is_empty()` or `x.len > 0`"; break;
    case T_INT:
    case T_FLOAT: hint = "; compare explicitly, e.g. `n != 0`"; break;
    default: hint = "; compare explicitly, e.g. `x != none`";
    }
    lc_panic("E0301", site, "condition must be bool, got %s%s", lc_kind(c, k), hint);
}

bool lc_contains(lc_v c, lc_v x, const char *site) {
    char k[64];
    lc_buf t = {0};
    switch (c.tag) {
    case T_LIST:
    case T_TUPLE:
    case T_HEAP: {
        lc_vec *v = VEC(c);
        for (int64_t i = 0; i < v->len; i++)
            if (lc_eq(lc_vget(v, i), x) == 1) return true;
        return false;
    }
    case T_SET: {
        if (lc_map_find(MAP(c), x)) return true;
        if (x.tag == T_FLOAT) {
            lc_map *m = MAP(c);
            for (int64_t i = 0; i < m->len; i++)
                if (lc_eq(m->e[i].k, x) == 1) return true;
        }
        return false;
    }
    case T_MAP: return lc_map_find(MAP(c), x) != NULL;
    case T_RANGE: {
        if (x.tag != T_INT) return false;
        lc_range *r = RANGE(c);
        int64_t n = x.u.i;
        if (r->step > 0) return n >= r->start && n < r->end && (n - r->start) % r->step == 0;
        return n <= r->start && n > r->end && (r->start - n) % (-r->step) == 0;
    }
    case T_STR:
        if (x.tag == T_STR) return memmem(STR(c)->data, STR(c)->len, STR(x)->data, STR(x)->len) != NULL || STR(x)->len == 0;
        lc_panic("E0301", site, "`in` on a string needs a str, got %s", lc_kind(x, k));
    case T_ERR: lc_panic("E0406", site, "`in` on an error (%s); add `?`", disp(c, &t));
    }
    lc_panic("E0301", site, "cannot use `in` with %s", lc_kind(c, k));
}

bool lc_compare(int op, lc_v a, lc_v b, const char *site) {
    char k1[64], k2[64];
    lc_buf t = {0}, u = {0};
    for (int i = 0; i < 2; i++) {
        lc_v v = i ? b : a;
        if (v.tag == T_ERR && (!(op == CMP_EQ || op == CMP_NE) || !(a.tag == T_ERR && b.tag == T_ERR)))
            lc_panic("E0406", site, "comparing an error (%s); add `?`", disp(v, &t));
    }
    switch (op) {
    case CMP_EQ:
    case CMP_NE: {
        int r = lc_eq(a, b);
        if (r < 0) lc_panic("E0301", site, "cannot compare %s with %s", lc_kind(a, k1), lc_kind(b, k2));
        return op == CMP_EQ ? r == 1 : r != 1;
    }
    case CMP_IN: return lc_contains(b, a, site);
    case CMP_NOTIN: return !lc_contains(b, a, site);
    }
    if (a.tag == T_NONE || b.tag == T_NONE) {
        static const char *syms[] = {"==", "!=", "<", "<=", ">", ">="};
        lc_panic("E0301", site, "cannot order none (%s %s %s)", short_repr(a, &t), syms[op], short_repr(b, &u));
    }
    bool ok;
    int c = lc_cmp(a, b, &ok);
    if (!ok) lc_panic("E0301", site, "cannot compare %s with %s", lc_kind(a, k1), lc_kind(b, k2));
    switch (op) {
    case CMP_LT: return c < 0;
    case CMP_LE: return c <= 0;
    case CMP_GT: return c > 0;
    default: return c >= 0;
    }
}

int64_t lc_len_slow(lc_v o, int field_id, const char *site) {
    bool found;
    lc_v r = lc_field(o, field_id, "len", &found, site);
    if (!found) r = lc_field_fallback(o, "len", M_len, site);
    int64_t n = lc_unbox_int(r, site);
    lc_release(r);
    return n;
}

_Noreturn void lc_unbox_fail(lc_v v, const char *want, const char *site) {
    char k[64];
    lc_panic("E0000", site, "internal error: the checker typed this %s, but it is %s", want, lc_kind(v, k));
}

int64_t lc_int_of(lc_v v, const char *site) {
    char k[64];
    if (v.tag == T_INT) return v.u.i;
    lc_panic("E0301", site, "want int got %s", lc_kind(v, k));
}

static bool norm_index(int64_t n, int64_t len, int64_t *out) {
    int64_t i = n < 0 ? len + n : n;
    if (i < 0 || i >= len) return false;
    *out = i;
    return true;
}

/* Byte offset of character `i` in a string. */
static int64_t char_off(lc_str *s, int64_t i) {
    if (s->nchars == s->len) return i;
    int64_t c = 0;
    for (int64_t b = 0; b < s->len; b++) {
        if (((unsigned char)s->data[b] & 0xC0) != 0x80) {
            if (c == i) return b;
            c++;
        }
    }
    return s->len;
}

static lc_v str_chars(lc_str *s, int64_t from, int64_t to) {
    int64_t a = char_off(s, from), b = char_off(s, to);
    return lc_str_new(s->data + a, b - a);
}

lc_v lc_index(lc_v o, lc_v i, const char *site) {
    char k1[64], k2[64];
    lc_buf t = {0};
    int64_t idx;
    if ((o.tag == T_LIST || o.tag == T_TUPLE) && i.tag == T_INT) {
        lc_vec *v = VEC(o);
        if (!norm_index(i.u.i, v->len, &idx)) lc_panic("E0402", site, "index %lld out of range for length %lld", (long long)i.u.i, (long long)v->len);
        return lc_retain(lc_vget(v, idx));
    }
    if (o.tag == T_STR && i.tag == T_INT) {
        lc_str *s = STR(o);
        if (!norm_index(i.u.i, s->nchars, &idx))
            lc_panic("E0402", site, "index %lld out of range for string of length %lld", (long long)i.u.i, (long long)s->nchars);
        return str_chars(s, idx, idx + 1);
    }
    if (o.tag == T_RANGE && i.tag == T_INT) {
        lc_range *r = RANGE(o);
        int64_t n = lc_range_len(r);
        if (!norm_index(i.u.i, n, &idx)) lc_panic("E0402", site, "index %lld out of range for length %lld", (long long)i.u.i, (long long)n);
        return lc_int(r->start + r->step * idx);
    }
    if (o.tag == T_MAP) {
        lc_entry *e = lc_map_find(MAP(o), i);
        if (!e) lc_panic("E0403", site, "key %s not found; use `m.get(k)` (gives `none`) or `m.get(k) ?? default`", short_repr(i, &t));
        return lc_retain(e->v);
    }
    if (o.tag == T_ERR) lc_panic("E0406", site, "indexing an error (%s); add `?`", disp(o, &t));
    if (o.tag == T_NONE) lc_panic("E0407", site, "indexing none");
    lc_panic("E0301", site, "cannot index %s with %s", lc_kind(o, k1), lc_kind(i, k2));
}

lc_v lc_slice(lc_v o, bool has_start, int64_t start, bool has_end, int64_t end, bool inclusive, const char *site) {
    char k[64];
    lc_buf t = {0};
    int64_t len;
    switch (o.tag) {
    case T_LIST:
    case T_TUPLE: len = VEC(o)->len; break;
    case T_STR: len = STR(o)->nchars; break;
    case T_RANGE: len = lc_range_len(RANGE(o)); break;
    case T_ERR: lc_panic("E0406", site, "slicing an error (%s); add `?`", disp(o, &t));
    default: lc_panic("E0301", site, "cannot slice %s", lc_kind(o, k));
    }
#define FIX(i) ((i) < 0 ? ((len + (i)) > 0 ? len + (i) : 0) : ((i) < len ? (i) : len))
    int64_t a = has_start ? FIX(start) : 0;
    int64_t b;
    if (!has_end) {
        b = len;
    } else if (inclusive) {
        b = FIX(end) + 1;
        if (b > len) b = len;
    } else {
        b = FIX(end);
    }
#undef FIX
    if (b < a) b = a;
    switch (o.tag) {
    case T_LIST:
    case T_TUPLE: {
        return lc_vec_slice(o, o.tag, a, b);
    }
    case T_STR: return str_chars(STR(o), a, b);
    default: {
        lc_range *r = RANGE(o);
        return lc_range_new(r->start + r->step * a, r->start + r->step * a + (b - a) * r->step, r->step);
    }
    }
}

lc_v lc_make_range(bool has_start, lc_v start, bool has_end, lc_v end, bool inclusive, const char *site) {
    int64_t s = has_start ? lc_int_of(start, site) : 0;
    if (!has_end) lc_panic("E0301", site, "a range needs an end (open ranges only work in slices like `xs[2..]`)");
    int64_t e = lc_int_of(end, site);
    if (inclusive) e += 1;
    return lc_range_new(s, e, 1);
}

lc_v lc_field(lc_v o, int field_id, const char *name, bool *found, const char *site) {
    *found = false;
    if (o.tag == T_STRUCT) {
        lc_rec *r = REC(o);
        const lc_struct_info *si = &lc_prog->structs[r->ty];
        for (int i = 0; i < si->nfields; i++) {
            if (si->field_ids[i] == field_id) {
                *found = true;
                return lc_retain(r->f[i]);
            }
        }
    } else if (o.tag == T_TUPLE && name[0] >= '0' && name[0] <= '9') {
        int64_t i = atoll(name);
        lc_vec *v = VEC(o);
        if (i >= v->len) lc_panic("E0402", site, "tuple has no element %lld (length %lld)", (long long)i, (long long)v->len);
        *found = true;
        return lc_retain(v->boxed[i]);
    }
    return LC_UNIT;
}

lc_v lc_field_fallback(lc_v o, const char *name, int method, const char *site) {
    char k[64];
    if (name[0] >= '0' && name[0] <= '9') lc_panic("E0301", site, "`.%s` needs a tuple, got %s", name, lc_kind(o, k));
    if (method >= 0) return lc_method(method, o, 0, NULL, site);
    lc_panic("E0203", site, "%s has no field `%s`", lc_kind(o, k), name);
}

/* ----- numbers from strings ----- */

void trim_ws(const char **s, int64_t *n);

/* Rust's `str::parse::<i64>()`: an optional sign and ASCII digits. */
static bool parse_i64(const char *s, int64_t n, int64_t *out) {
    if (n == 0) return false;
    int64_t i = 0;
    bool neg = false;
    if (s[0] == '+' || s[0] == '-') {
        neg = s[0] == '-';
        i = 1;
        if (n == 1) return false;
    }
    uint64_t v = 0;
    for (; i < n; i++) {
        if (s[i] < '0' || s[i] > '9') return false;
        uint64_t d = (uint64_t)(s[i] - '0');
        if (v > (UINT64_MAX - d) / 10) return false;
        v = v * 10 + d;
    }
    if (neg) {
        if (v > (uint64_t)INT64_MAX + 1) return false;
        *out = (int64_t)(0 - v);
    } else {
        if (v > (uint64_t)INT64_MAX) return false;
        *out = (int64_t)v;
    }
    return true;
}

/* Rust's `str::parse::<f64>()`: decimal digits with an optional point and
 * exponent, or inf/infinity/nan; no hex, no spaces. */
static bool parse_f64(const char *s, int64_t n, double *out) {
    if (n == 0) return false;
    int64_t i = 0;
    if (s[i] == '+' || s[i] == '-') i++;
    const char *rest = s + i;
    int64_t rn = n - i;
    if ((rn == 3 && strncasecmp(rest, "inf", 3) == 0) || (rn == 8 && strncasecmp(rest, "infinity", 8) == 0) || (rn == 3 && strncasecmp(rest, "nan", 3) == 0)) {
    } else {
        int64_t digits = 0;
        while (i < n && s[i] >= '0' && s[i] <= '9') i++, digits++;
        if (i < n && s[i] == '.') {
            i++;
            while (i < n && s[i] >= '0' && s[i] <= '9') i++, digits++;
        }
        if (digits == 0) return false;
        if (i < n && (s[i] == 'e' || s[i] == 'E')) {
            i++;
            if (i < n && (s[i] == '+' || s[i] == '-')) i++;
            int64_t ed = 0;
            while (i < n && s[i] >= '0' && s[i] <= '9') i++, ed++;
            if (ed == 0) return false;
        }
        if (i != n) return false;
    }
    char *tmp = malloc(n + 1);
    memcpy(tmp, s, n);
    tmp[n] = 0;
    *out = strtod(tmp, NULL);
    free(tmp);
    return true;
}

/* `parse()` on a string: an int, a float, a bool, or an error. */
lc_v lc_parse_number(lc_str *str, bool *ok) {
    const char *s = str->data;
    int64_t n = str->len;
    trim_ws(&s, &n);
    *ok = true;
    int64_t i;
    if (parse_i64(s, n, &i)) return lc_int(i);
    if (n > 0 && s[0] != '_' && s[n - 1] != '_') {
        char *tmp = malloc(n + 1);
        int64_t m = 0;
        for (int64_t j = 0; j < n; j++)
            if (s[j] != '_') tmp[m++] = s[j];
        bool had = m != n;
        bool r = had && parse_i64(tmp, m, &i);
        free(tmp);
        if (r) return lc_int(i);
    }
    double f;
    if (parse_f64(s, n, &f)) {
        bool special = false;
        for (int64_t j = 0; j < n; j++)
            if (s[j] == 'n' || s[j] == 'N' || s[j] == 'i' || s[j] == 'I') special = true;
        if (n > 0 && !special) return lc_float(f);
    }
    *ok = false;
    return LC_UNIT;
}

static lc_v parse_err(lc_str *s, const char *what) {
    lc_buf b = {0};
    lc_buf_puts(&b, "cannot parse ");
    lc_v sv = lc_obj_v(T_STR, s);
    lc_buf_repr(&b, sv);
    lc_buf_puts(&b, " as ");
    lc_buf_puts(&b, what);
    return lc_err_new(lc_buf_finish(&b));
}

lc_v lc_convert(lc_v v, int to, const char *name, int64_t lo, int64_t hi, const char *site) {
    char k[64];
    lc_buf t = {0};
    if (v.tag == T_ERR) lc_panic("E0406", site, "converting an error (%s); add `?`", disp(v, &t));
    switch (to) {
    case CONV_INT: {
        int64_t n;
        if (v.tag == T_INT) {
            n = v.u.i;
        } else if (v.tag == T_FLOAT) {
            double f = v.u.f;
            if (!isfinite(f) || f >= 9.3e18 || f <= -9.3e18) {
                lc_fmt_float(&t, f);
                lc_panic("E0405", site, "%s does not fit in %s", t.p, name);
            }
            n = (int64_t)trunc(f);
        } else if (v.tag == T_BOOL) {
            n = v.u.i;
        } else if (v.tag == T_STR) {
            const char *s = STR(v)->data;
            int64_t len = STR(v)->len;
            trim_ws(&s, &len);
            if (!parse_i64(s, len, &n)) {
                double f;
                if (len > 0 && parse_f64(s, len, &f) && f == trunc(f) && fabs(f) < 9e18) {
                    n = (int64_t)f;
                } else {
                    return parse_err(STR(v), name);
                }
            }
        } else {
            lc_panic("E0301", site, "cannot convert %s to %s", lc_kind(v, k), name);
        }
        if (n < lo || n > hi) lc_panic("E0405", site, "%lld does not fit in %s", (long long)n, name);
        return lc_int(n);
    }
    case CONV_FLOAT:
        if (v.tag == T_INT) return lc_float((double)v.u.i);
        if (v.tag == T_FLOAT) return v;
        if (v.tag == T_BOOL) return lc_float((double)v.u.i);
        if (v.tag == T_STR) {
            const char *s = STR(v)->data;
            int64_t len = STR(v)->len;
            trim_ws(&s, &len);
            double f;
            if (len > 0 && parse_f64(s, len, &f)) return lc_float(f);
            return parse_err(STR(v), "f64");
        }
        lc_panic("E0301", site, "cannot convert %s to f64", lc_kind(v, k));
    case CONV_STR: return lc_display(v);
    default:
        if (v.tag == T_BOOL) return v;
        if (v.tag == T_INT) return lc_bool(v.u.i != 0);
        if (v.tag == T_STR) {
            const char *s = STR(v)->data;
            int64_t len = STR(v)->len;
            trim_ws(&s, &len);
            if (len == 4 && memcmp(s, "true", 4) == 0) return LC_TRUE;
            if (len == 5 && memcmp(s, "false", 5) == 0) return LC_FALSE;
            return parse_err(STR(v), "bool");
        }
        lc_panic("E0301", site, "cannot convert %s to bool", lc_kind(v, k));
    }
}

lc_v lc_parse_as(lc_v v, int to, const char *name, int64_t lo, int64_t hi, const char *site) {
    lc_v r = lc_method(M_parse, v, 0, NULL, site);
    if (v.tag != T_STR || lc_unwinding) return r;
    switch (to) {
    case CONV_INT:
        if (r.tag == T_INT && r.u.i >= lo && r.u.i <= hi) return r;
        break;
    case CONV_FLOAT:
        if (r.tag == T_FLOAT) return r;
        if (r.tag == T_INT) return lc_float((double)r.u.i);
        break;
    case CONV_BOOL:
        if (r.tag == T_BOOL) return r;
        break;
    default:
        return r;
    }
    lc_release(r);
    return parse_err(STR(v), name);
}

/* ----- declared types ----- */

const char *lc_ty_name(const lc_ty *t, char *buf, size_t len) {
    lc_buf b = {0};
    char x[256], y[256];
    switch (t->kind) {
    case TY_ANY: lc_buf_puts(&b, t->name ? t->name : "any"); break;
    case TY_INT: lc_buf_puts(&b, t->name); break;
    case TY_FLOAT: lc_buf_puts(&b, "f64"); break;
    case TY_BOOL: lc_buf_puts(&b, "bool"); break;
    case TY_STR: lc_buf_puts(&b, "str"); break;
    case TY_UNIT: lc_buf_puts(&b, "()"); break;
    case TY_LIST:
        lc_buf_putc(&b, '[');
        lc_buf_puts(&b, lc_ty_name(t->a, x, sizeof x));
        lc_buf_putc(&b, ']');
        break;
    case TY_MAP:
        lc_buf_putc(&b, '{');
        lc_buf_puts(&b, lc_ty_name(t->a, x, sizeof x));
        lc_buf_putc(&b, ':');
        lc_buf_puts(&b, lc_ty_name(t->b, y, sizeof y));
        lc_buf_putc(&b, '}');
        break;
    case TY_SET:
        lc_buf_putc(&b, '{');
        lc_buf_puts(&b, lc_ty_name(t->a, x, sizeof x));
        lc_buf_putc(&b, '}');
        break;
    case TY_HEAP:
        lc_buf_puts(&b, "heap[");
        lc_buf_puts(&b, lc_ty_name(t->a, x, sizeof x));
        lc_buf_putc(&b, ']');
        break;
    case TY_TUPLE:
        lc_buf_putc(&b, '(');
        for (int i = 0; i < t->n; i++) {
            if (i) lc_buf_puts(&b, ", ");
            lc_buf_puts(&b, lc_ty_name(t->elems[i], x, sizeof x));
        }
        lc_buf_putc(&b, ')');
        break;
    case TY_OPT:
        lc_buf_puts(&b, lc_ty_name(t->a, x, sizeof x));
        lc_buf_putc(&b, '?');
        break;
    case TY_RES:
        lc_buf_puts(&b, lc_ty_name(t->a, x, sizeof x));
        lc_buf_putc(&b, '!');
        break;
    case TY_FN:
        lc_buf_puts(&b, "fn(");
        for (int i = 0; i < t->n; i++) {
            if (i) lc_buf_puts(&b, ", ");
            lc_buf_puts(&b, lc_ty_name(t->elems[i], x, sizeof x));
        }
        lc_buf_putc(&b, ')');
        if (t->a && t->a->kind != TY_UNIT) {
            lc_buf_putc(&b, ' ');
            lc_buf_puts(&b, lc_ty_name(t->a, x, sizeof x));
        }
        break;
    case TY_STRUCT:
    case TY_ENUM:
        lc_buf_puts(&b, t->kind == TY_STRUCT ? lc_prog->structs[t->id].name : lc_prog->enums[t->id].name);
        if (t->n) {
            lc_buf_putc(&b, '[');
            for (int i = 0; i < t->n; i++) {
                if (i) lc_buf_puts(&b, ", ");
                lc_buf_puts(&b, lc_ty_name(t->elems[i], x, sizeof x));
            }
            lc_buf_putc(&b, ']');
        }
        break;
    }
    snprintf(buf, len, "%s", b.p ? b.p : "");
    free(b.p);
    return buf;
}

bool lc_ty_matches(lc_v v, const lc_ty *t) {
    switch (t->kind) {
    case TY_ANY: return true;
    case TY_INT: return v.tag == T_INT;
    case TY_FLOAT: return v.tag == T_FLOAT || v.tag == T_INT;
    case TY_BOOL: return v.tag == T_BOOL;
    case TY_STR: return v.tag == T_STR;
    case TY_UNIT: return v.tag == T_UNIT;
    case TY_LIST: return v.tag == T_LIST || v.tag == T_RANGE;
    case TY_MAP: return v.tag == T_MAP;
    case TY_SET: return v.tag == T_SET;
    case TY_HEAP: return v.tag == T_HEAP;
    case TY_TUPLE: return v.tag == T_TUPLE && VEC(v)->len == t->n;
    case TY_OPT: return v.tag == T_NONE || lc_ty_matches(v, t->a);
    case TY_RES: return v.tag == T_ERR || lc_ty_matches(v, t->a);
    case TY_FN: return v.tag == T_FUNC;
    case TY_STRUCT: return v.tag == T_STRUCT && REC(v)->ty == (uint32_t)t->id;
    case TY_ENUM: return v.tag == T_VARIANT && REC(v)->ty == (uint32_t)t->id;
    }
    return false;
}

bool lc_coerce(lc_v *v, const lc_ty *t, char *got, size_t gotlen) {
    lc_v x = *v;
    char k[64];
    switch (t->kind) {
    case TY_ANY: return true;
    case TY_INT:
        if (x.tag == T_INT) {
            if (x.u.i < t->lo || x.u.i > t->hi) {
                snprintf(got, gotlen, "%lld (out of range for %s)", (long long)x.u.i, t->name);
                return false;
            }
            return true;
        }
        break;
    case TY_FLOAT:
        if (x.tag == T_INT) {
            *v = lc_float((double)x.u.i);
            return true;
        }
        break;
    case TY_LIST:
        if (t->a->kind == TY_FLOAT && x.tag == T_LIST && VEC(x)->len > 0 && lc_vget(VEC(x), 0).tag == T_INT) {
            lc_vec *xs = VEC(x);
            lc_v out = lc_list_new(xs->len);
            for (int64_t i = 0; i < xs->len; i++) {
                lc_v e = lc_vget(xs, i);
                lc_vec_push(out, e.tag == T_INT ? lc_float((double)e.u.i) : lc_retain(e));
            }
            lc_release(x);
            *v = out;
            return true;
        }
        if (x.tag == T_RANGE) {
            lc_v out = lc_items(x, "");
            lc_release(x);
            *v = out;
            return true;
        }
        break;
    case TY_OPT:
        if (x.tag == T_NONE) return true;
        return lc_coerce(v, t->a, got, gotlen);
    case TY_RES:
        if (x.tag == T_ERR) return true;
        return lc_coerce(v, t->a, got, gotlen);
    }
    if (x.tag == T_ERR) {
        lc_buf b = {0};
        snprintf(got, gotlen, "an error (%s); add `?`", disp(x, &b));
        free(b.p);
        return false;
    }
    if (lc_ty_matches(x, t)) return true;
    snprintf(got, gotlen, "%s", lc_kind(x, k));
    return false;
}

lc_v lc_coerce_arg(lc_v v, const lc_ty *t, const char *param, const char *fn, const char *site) {
    char got[256], want[256];
    if (!lc_coerce(&v, t, got, sizeof got))
        lc_panic("E0301", site, "argument `%s` of `%s`: want %s got %s", param, fn, lc_ty_name(t, want, sizeof want), got);
    return v;
}

lc_v lc_coerce_ret(lc_v v, const lc_ty *t, const char *fn, const char *site) {
    char got[256], want[256];
    if (t->kind == TY_UNIT || (t->kind == TY_RES && t->a->kind == TY_UNIT)) {
        if (v.tag == T_ERR) return v;
        lc_release(v);
        return LC_UNIT;
    }
    if (!lc_coerce(&v, t, got, sizeof got)) {
        if (strcmp(got, "()") == 0)
            lc_panic("E0301", site, "`%s` must return %s but its body ended without a value; end with an expression or use `return`", fn,
                     lc_ty_name(t, want, sizeof want));
        lc_panic("E0301", site, "`%s` returned %s, want %s", fn, got, lc_ty_name(t, want, sizeof want));
    }
    return v;
}

lc_v lc_coerce_field(lc_v v, const lc_ty *t, const char *field, const char *owner, bool variant, const char *site) {
    char got[256], want[256];
    if (!lc_coerce(&v, t, got, sizeof got)) {
        if (variant) lc_panic("E0301", site, "`%s` field: want %s got %s", owner, lc_ty_name(t, want, sizeof want), got);
        lc_panic("E0301", site, "field `%s` of %s: want %s got %s", field, owner, lc_ty_name(t, want, sizeof want), got);
    }
    return v;
}

_Noreturn void lc_no_arm(lc_v v, const char *site) {
    lc_buf t = {0};
    lc_panic("E0411", site, "no match arm for %s", short_repr(v, &t));
}

_Noreturn void lc_bad_unpack(lc_v v, int n, bool in_for, const char *site) {
    lc_buf t = {0};
    if (n < 0) lc_panic("E0414", site, "cannot destructure %s%s", short_repr(v, &t), in_for ? " in `for`" : "");
    lc_panic("E0414", site, "cannot unpack %s into %d names", short_repr(v, &t), n);
}

void lc_check_ignored(lc_v v, const char *site) {
    lc_buf t = {0};
    if (v.tag == T_ERR)
        lc_panic("E0406", site, "error ignored: %s; add `?` to pass it up, or handle it with `??` or `match`", disp(v, &t));
}

lc_v lc_try_none(bool fn_optional) {
    if (fn_optional) return LC_NONE;
    return lc_err_new(lc_cstr("unexpected none"));
}

/* The first place two values differ, as a path like `[2].age`, with both
 * sides there. Mirrors `value::first_diff`. */
static bool first_diff(lc_v a, lc_v b, lc_buf *path, lc_buf *x, lc_buf *y) {
    if (lc_eq(a, b) == 1) return false;
    char t[64];
    if ((a.tag == T_LIST && b.tag == T_LIST) || (a.tag == T_TUPLE && b.tag == T_TUPLE)) {
        lc_vec *p = VEC(a), *q = VEC(b);
        for (int64_t i = 0; i < p->len && i < q->len; i++) {
            int64_t n = path->len;
            snprintf(t, sizeof t, a.tag == T_LIST ? "[%lld]" : ".%lld", (long long)i);
            lc_buf_puts(path, t);
            if (first_diff(lc_vget(p, i), lc_vget(q, i), path, x, y)) return true;
            path->len = n;
            if (path->p) path->p[n] = 0;
        }
        if (p->len != q->len) {
            lc_buf_puts(path, ".len");
            snprintf(t, sizeof t, "%lld", (long long)p->len);
            lc_buf_puts(x, t);
            snprintf(t, sizeof t, "%lld", (long long)q->len);
            lc_buf_puts(y, t);
            return true;
        }
    } else if (a.tag == T_STRUCT && b.tag == T_STRUCT && REC(a)->ty == REC(b)->ty) {
        lc_rec *p = REC(a), *q = REC(b);
        const lc_struct_info *si = &lc_prog->structs[p->ty];
        for (int64_t i = 0; i < p->n && i < q->n; i++) {
            int64_t n = path->len;
            lc_buf_putc(path, '.');
            lc_buf_puts(path, si->field_names[i]);
            if (first_diff(p->f[i], q->f[i], path, x, y)) return true;
            path->len = n;
            if (path->p) path->p[n] = 0;
        }
    } else if (a.tag == T_MAP && b.tag == T_MAP) {
        lc_map *p = MAP(a), *q = MAP(b);
        for (int64_t i = 0; i < p->len; i++) {
            int64_t n = path->len;
            lc_buf_putc(path, '[');
            lc_buf_repr(path, p->e[i].k);
            lc_buf_putc(path, ']');
            lc_entry *e = lc_map_find(q, p->e[i].k);
            if (!e) {
                lc_buf_repr(x, p->e[i].v);
                lc_buf_puts(y, "(missing)");
                return true;
            }
            if (first_diff(p->e[i].v, e->v, path, x, y)) return true;
            path->len = n;
            if (path->p) path->p[n] = 0;
        }
        for (int64_t i = 0; i < q->len; i++) {
            if (!lc_map_find(p, q->e[i].k)) {
                lc_buf_putc(path, '[');
                lc_buf_repr(path, q->e[i].k);
                lc_buf_putc(path, ']');
                lc_buf_puts(x, "(missing)");
                lc_buf_repr(y, q->e[i].v);
                return true;
            }
        }
    }
    lc_buf_repr(x, a);
    lc_buf_repr(y, b);
    return true;
}

bool lc_assert_eq(lc_v a, lc_v b, lc_v msg, bool has_msg, const char *site) {
    char k1[64], k2[64];
    int r = lc_eq(a, b);
    if (r < 0) lc_panic("E0301", site, "cannot compare %s with %s", lc_kind(a, k1), lc_kind(b, k2));
    if (r == 1) return true;
    lc_buf x = {0}, y = {0}, m = {0};
    lc_buf_repr(&x, a);
    lc_buf_repr(&y, b);
    if (has_msg) {
        lc_buf_display(&m, msg);
        lc_buf_putc(&m, ' ');
    }
    lc_buf path = {0}, dx = {0}, dy = {0};
    lc_buf_puts(&path, "");
    first_diff(a, b, &path, &dx, &dy);
    if (path.len > 0)
        lc_panic("E0420", site, "%sleft != right\n  left:  %s\n  right: %s\n  first difference at %s: %s vs %s", m.p ? m.p : "", x.p ? x.p : "",
                 y.p ? y.p : "", path.p, dx.p ? dx.p : "", dy.p ? dy.p : "");
    lc_panic("E0420", site, "%sleft != right\n  left:  %s\n  right: %s", m.p ? m.p : "", x.p ? x.p : "", y.p ? y.p : "");
}

_Noreturn void lc_assert_fail(lc_v msg, bool has_msg, const char *site) {
    lc_buf m = {0};
    if (has_msg) lc_panic("E0421", site, "assertion failed: %s", disp(msg, &m));
    lc_panic("E0421", site, "assertion failed");
}

/* ----- places ----- */

static lc_v clone_obj(lc_v v) {
    switch (v.tag) {
    case T_STR: return lc_str_new(STR(v)->data, STR(v)->len);
    case T_LIST:
    case T_TUPLE:
    case T_HEAP: {
        return lc_vec_slice(v, v.tag, 0, VEC(v)->len);
    }
    case T_MAP:
    case T_SET: {
        lc_map *m = MAP(v);
        lc_v out = v.tag == T_MAP ? lc_map_new() : lc_set_new();
        for (int64_t i = 0; i < m->len; i++) lc_map_put(out, lc_retain(m->e[i].k), lc_retain(m->e[i].v));
        return out;
    }
    case T_STRUCT:
    case T_VARIANT: {
        lc_rec *r = REC(v);
        lc_v out = lc_rec_new(v.tag, r->ty, r->tag, r->n, r->f);
        for (int64_t i = 0; i < r->n; i++) lc_retain(r->f[i]);
        return out;
    }
    default: return lc_retain(v);
    }
}

void lc_make_unique(lc_v *p) {
    if (p->tag >= T_STR && p->tag != T_FUNC && p->u.o->rc > 1) {
        lc_v c = clone_obj(*p);
        lc_release(*p);
        *p = c;
    }
}

lc_v *lc_place_field(lc_v *p, int field_id, const char *name, const char *site) {
    char k[64];
    if (p->tag == T_STRUCT) {
        lc_make_unique(p);
        lc_rec *r = REC(*p);
        const lc_struct_info *si = &lc_prog->structs[r->ty];
        for (int i = 0; i < si->nfields; i++)
            if (si->field_ids[i] == field_id) return &r->f[i];
        lc_panic("E0208", site, "%s has no field `%s`", si->name, name);
    }
    if (p->tag == T_TUPLE) {
        if (name[0] >= '0' && name[0] <= '9') {
            int64_t i = atoll(name);
            if (i < VEC(*p)->len) {
                lc_make_unique(p);
                return &VEC(*p)->boxed[i];
            }
        }
        lc_panic("E0402", site, "tuple has no element %s", name);
    }
    lc_panic("E0301", site, "cannot set field `%s` on %s", name, lc_kind(*p, k));
}

static lc_v zero_of(lc_v v, bool *ok) {
    *ok = true;
    switch (v.tag) {
    case T_INT: return lc_int(0);
    case T_FLOAT: return lc_float(0);
    case T_STR: return lc_str_new("", 0);
    case T_LIST: return lc_list_new(0);
    case T_SET: return lc_set_new();
    case T_MAP: return lc_map_new();
    }
    *ok = false;
    return LC_UNIT;
}

lc_v *lc_place_index(lc_v *p, lc_v key, int viv, lc_v zero, int method, const char *site) {
    char k[64];
    lc_buf t = {0};
    if (p->tag == T_LIST) {
        if (key.tag != T_INT) lc_panic("E0301", site, "list index must be int, got %s", lc_kind(key, k));
        int64_t idx, len = VEC(*p)->len;
        if (!norm_index(key.u.i, len, &idx)) lc_panic("E0402", site, "index %lld out of range for length %lld", (long long)key.u.i, (long long)len);
        lc_make_unique(p);
        lc_vec_box(VEC(*p));
        return &VEC(*p)->boxed[idx];
    }
    if (p->tag == T_MAP) {
        lc_make_unique(p);
        lc_entry *e = lc_map_find(MAP(*p), key);
        if (!e) {
            lc_v created;
            bool ok = true;
            switch (viv) {
            case VIV_INSERT: created = LC_UNIT; break;
            case VIV_ZERO_OF:
                created = zero_of(zero, &ok);
                if (!ok) lc_panic("E0403", site, "key %s not found", short_repr(key, &t));
                break;
            case VIV_METHOD: created = method == M_add ? lc_set_new() : lc_list_new(0); break;
            case VIV_MAP: created = lc_map_new(); break;
            default: lc_panic("E0403", site, "key %s not found", short_repr(key, &t));
            }
            bool existed;
            extern lc_entry *lc_map_insert_entry(lc_v map, lc_v k, lc_v v, bool *existed);
            e = lc_map_insert_entry(*p, lc_retain(key), created, &existed);
        }
        return &e->v;
    }
    if (p->tag == T_STR) lc_panic("E0410", site, "strings are immutable; build a new string (e.g. with slices and `+`)");
    lc_panic("E0301", site, "cannot index into %s", lc_kind(*p, k));
}

void lc_store_index(lc_v *p, lc_v key, lc_v v, const char *site) {
    int64_t idx;
    if (p->tag == T_LIST && key.tag == T_INT && norm_index(key.u.i, VEC(*p)->len, &idx)) {
        lc_make_unique(p);
        lc_vset(VEC(*p), idx, v);
        return;
    }
    lc_set(lc_place_index(p, key, VIV_INSERT, LC_UNIT, 0, site), v);
}

void lc_update_index(lc_v *p, lc_v key, int op, lc_v v, const char *site, const char *op_site) {
    int64_t idx;
    if (p->tag == T_LIST && key.tag == T_INT && norm_index(key.u.i, VEC(*p)->len, &idx)) {
        lc_make_unique(p);
        lc_vec *x = VEC(*p);
        if (x->kind == K_BOXED) {
            /* Taken out first, so a unique string or list grows in place. */
            lc_v old = lc_take(&x->boxed[idx]);
            x->boxed[idx] = lc_arith_own(op, old, v, op_site);
        } else {
            lc_vset(x, idx, lc_arith_own(op, lc_vget(x, idx), v, op_site));
        }
        return;
    }
    lc_v *e = lc_place_index(p, key, VIV_ZERO_OF, v, 0, site);
    lc_v old = lc_take(e);
    *e = lc_arith_own(op, old, v, op_site);
}

/* ----- iteration ----- */


void lc_iter_init(lc_iter *it, lc_v v, bool keys_only, const char *site) {
    char k[64];
    lc_buf t = {0};
    it->src = lc_retain(v);
    it->sorted = LC_UNIT;
    it->i = 0;
    switch (v.tag) {
    case T_RANGE:
        it->kind = IT_RANGE;
        it->n = lc_range_len(RANGE(v));
        it->start = RANGE(v)->start;
        it->step = RANGE(v)->step;
        return;
    case T_LIST:
    case T_TUPLE:
        it->kind = IT_VEC;
        it->n = VEC(v)->len;
        return;
    case T_HEAP:
        it->kind = IT_VEC;
        it->sorted = lc_heap_sorted(v);
        lc_release(it->src);
        it->src = it->sorted;
        it->sorted = LC_UNIT;
        it->n = VEC(it->src)->len;
        return;
    case T_SET:
        it->kind = IT_KEYS;
        it->n = MAP(v)->len;
        return;
    case T_MAP:
        it->kind = keys_only ? IT_KEYS : IT_PAIRS;
        it->n = MAP(v)->len;
        return;
    case T_STR:
        it->kind = IT_STR;
        it->n = STR(v)->len;
        return;
    case T_ERR: lc_panic("E0406", site, "cannot loop over an error (%s); add `?`", disp(v, &t));
    }
    lc_panic("E0301", site, "cannot loop over %s", lc_kind(v, k));
}

bool lc_iter_next(lc_iter *it, lc_v *out) {
    if (it->i >= it->n) return false;
    switch (it->kind) {
    case IT_RANGE: *out = lc_int(it->start + it->step * it->i++); return true;
    case IT_VEC: *out = lc_retain(lc_vget(VEC(it->src), it->i++)); return true;
    case IT_KEYS: *out = lc_retain(MAP(it->src)->e[it->i++].k); return true;
    case IT_PAIRS: {
        lc_entry *e = &MAP(it->src)->e[it->i++];
        lc_v pair[2] = {lc_retain(e->k), lc_retain(e->v)};
        *out = lc_tuple_of(2, pair);
        return true;
    }
    default: {
        lc_str *s = STR(it->src);
        int64_t a = it->i, b = a + 1;
        while (b < s->len && ((unsigned char)s->data[b] & 0xC0) == 0x80) b++;
        it->i = b;
        *out = lc_str_new(s->data + a, b - a);
        return true;
    }
    }
}

void lc_iter_done(lc_iter *it) { lc_release(it->src); }

lc_v lc_items(lc_v v, const char *site) {
    char k[64];
    switch (v.tag) {
    case T_LIST: return lc_retain(v);
    case T_TUPLE: return lc_vec_slice(v, T_LIST, 0, VEC(v)->len);
    case T_HEAP: return lc_heap_sorted(v);
    case T_SET:
    case T_MAP:
    case T_RANGE:
    case T_STR: {
        lc_iter it;
        lc_iter_init(&it, v, false, site);
        lc_v out = lc_list_new(it.n);
        lc_v x;
        while (lc_iter_next(&it, &x)) lc_vec_push(out, x);
        lc_iter_done(&it);
        return out;
    }
    }
    lc_panic("E0301", site, "cannot iterate over %s", lc_kind(v, k));
}

/* ----- calls ----- */

lc_v lc_call(lc_v f, int argc, lc_v *args, const char *site) {
    char k[64];
    if (f.tag != T_FUNC) lc_panic("E0301", site, "cannot call %s", lc_kind(f, k));
    lc_fn *fn = FN(f);
    switch (fn->kind) {
    case FN_USER:
    case FN_CTOR: return fn->code(fn, argc, args, site);
    case FN_CLOSURE: {
        int np = fn->nparams;
        if (np > 1 && argc == 1 && args[0].tag == T_TUPLE && VEC(args[0])->len == np) {
            lc_v *spread = VEC(args[0])->boxed;
            lc_enter_lambda(site);
            lc_v r = fn->code(fn, np, spread, site);
            lc_depth--;
            return r;
        }
        if (argc != np) lc_panic("E0206", site, "lambda takes %d argument(s), got %d", np, argc);
        lc_enter_lambda(site);
        lc_v r = fn->code(fn, argc, args, site);
        lc_depth--;
        return r;
    }
    case FN_METHOD:
        if (argc == 0) lc_panic("E0206", site, "`%s` needs a receiver", lc_method_names[fn->id]);
        return lc_method(fn->id, args[0], argc - 1, args + 1, site);
    default: {
        if (argc == 0) lc_panic("E0206", site, "`%s` needs a receiver", lc_convs[fn->id].name);
        const lc_conv_info *c = &lc_convs[fn->id];
        return lc_convert(args[0], c->to, c->name, c->lo, c->hi, site);
    }
    }
}

/* ----- format specs ----- */

static int64_t utf8_len(const char *s, int64_t n) {
    int64_t c = 0;
    for (int64_t i = 0; i < n; i++)
        if (((unsigned char)s[i] & 0xC0) != 0x80) c++;
    return c;
}

static void put_fill(lc_buf *b, int fill, int64_t n) {
    char enc[4];
    int len;
    if (fill < 0x80) {
        enc[0] = (char)fill;
        len = 1;
    } else if (fill < 0x800) {
        enc[0] = (char)(0xC0 | (fill >> 6));
        enc[1] = (char)(0x80 | (fill & 0x3F));
        len = 2;
    } else if (fill < 0x10000) {
        enc[0] = (char)(0xE0 | (fill >> 12));
        enc[1] = (char)(0x80 | ((fill >> 6) & 0x3F));
        enc[2] = (char)(0x80 | (fill & 0x3F));
        len = 3;
    } else {
        enc[0] = (char)(0xF0 | (fill >> 18));
        enc[1] = (char)(0x80 | ((fill >> 12) & 0x3F));
        enc[2] = (char)(0x80 | ((fill >> 6) & 0x3F));
        enc[3] = (char)(0x80 | (fill & 0x3F));
        len = 4;
    }
    for (int64_t i = 0; i < n; i++) lc_buf_put(b, enc, len);
}

static int fill_char(const lc_fmt *f) { return f->fill >= 0 ? f->fill : (f->zero ? '0' : ' '); }

static void pad(lc_buf *out, const lc_fmt *f, const char *body, int64_t blen, bool numeric) {
    int64_t len = utf8_len(body, blen);
    if (len >= f->width) {
        lc_buf_put(out, body, blen);
        return;
    }
    int64_t n = f->width - len;
    char align = f->align ? f->align : (numeric ? '>' : '<');
    int fill = fill_char(f);
    if (align == '>' || align == '=') {
        put_fill(out, fill, n);
        lc_buf_put(out, body, blen);
    } else if (align == '^') {
        put_fill(out, fill, n / 2);
        lc_buf_put(out, body, blen);
        put_fill(out, fill, n - n / 2);
    } else {
        lc_buf_put(out, body, blen);
        put_fill(out, fill, n);
    }
}

static void number(lc_buf *out, const lc_fmt *f, bool neg, const char *prefix, const char *digits) {
    const char *sign = neg ? "-" : f->sign == '+' ? "+" : f->sign == ' ' ? " " : "";
    int64_t len = (int64_t)strlen(sign) + (int64_t)strlen(prefix) + utf8_len(digits, (int64_t)strlen(digits));
    if (len < f->width && (f->align == '=' || (f->zero && !f->align))) {
        lc_buf_puts(out, sign);
        lc_buf_puts(out, prefix);
        put_fill(out, f->fill >= 0 ? f->fill : (f->zero ? '0' : ' '), f->width - len);
        lc_buf_puts(out, digits);
        return;
    }
    lc_buf b = {0};
    lc_buf_puts(&b, sign);
    lc_buf_puts(&b, prefix);
    lc_buf_puts(&b, digits);
    pad(out, f, b.p, b.len, true);
    free(b.p);
}

static void group_digits(lc_buf *out, const char *d, int64_t n, char sep, int size) {
    for (int64_t i = 0; i < n; i++) {
        if (i > 0 && (n - i) % size == 0) lc_buf_putc(out, sep);
        lc_buf_putc(out, d[i]);
    }
}

static void text(lc_buf *out, const lc_fmt *f, const char *s, int64_t n) {
    if (f->precision >= 0) {
        int64_t c = 0, i = 0;
        for (; i < n; i++) {
            if (((unsigned char)s[i] & 0xC0) != 0x80) {
                if (c == f->precision) break;
                c++;
            }
        }
        n = i;
    }
    pad(out, f, s, n, false);
}

/* `1.234568e+03`, as Python writes it. */
static void exp_form(lc_buf *b, double a, int64_t p) {
    char t[512];
    snprintf(t, sizeof t, "%.*e", (int)p, a);
    lc_buf_puts(b, t);
}

static void trim_g(lc_buf *b, bool alt) {
    if (alt || !memchr(b->p, '.', b->len)) return;
    while (b->len && b->p[b->len - 1] == '0') b->len--;
    if (b->len && b->p[b->len - 1] == '.') b->len--;
    b->p[b->len] = 0;
}

static void general(lc_buf *out, double a, int64_t p, bool alt) {
    if (p < 1) p = 1;
    char t[512];
    snprintf(t, sizeof t, "%.*e", (int)(p - 1), a);
    int exp = atoi(strchr(t, 'e') + 1);
    lc_buf b = {0};
    if (a == 0.0 || (exp >= -4 && exp < p)) {
        int64_t prec = p - 1 - exp;
        if (prec < 0) prec = 0;
        snprintf(t, sizeof t, "%.*f", (int)prec, a);
        lc_buf_puts(&b, t);
        trim_g(&b, alt);
        lc_buf_put(out, b.p, b.len);
    } else {
        exp_form(&b, a, p - 1);
        char *e = strchr(b.p, 'e');
        lc_buf m = {0};
        lc_buf_put(&m, b.p, e - b.p);
        trim_g(&m, alt);
        lc_buf_put(out, m.p, m.len);
        lc_buf_puts(out, e);
        free(m.p);
    }
    free(b.p);
}

static void fmt_float_spec(lc_buf *out, const lc_fmt *f, double x, const char *plain) {
    bool neg = signbit(x) && !isnan(x);
    double a = fabs(x);
    bool upper = f->kind == 'E' || f->kind == 'F' || f->kind == 'G';
    lc_buf body = {0};
    char t[512];
    if (!isfinite(a)) {
        lc_buf_puts(&body, isnan(a) ? "nan" : "inf");
    } else {
        int64_t p = f->precision;
        switch (f->kind) {
        case 'f':
        case 'F':
            snprintf(t, sizeof t, "%.*f", (int)(p >= 0 ? p : 6), a);
            lc_buf_puts(&body, t);
            break;
        case 'e':
        case 'E': exp_form(&body, a, p >= 0 ? p : 6); break;
        case 'g':
        case 'G':
        case 'n': general(&body, a, p >= 0 ? p : 6, f->alt); break;
        case '%':
            snprintf(t, sizeof t, "%.*f%%", (int)(p >= 0 ? p : 6), a * 100.0);
            lc_buf_puts(&body, t);
            break;
        case 'd': lc_fmt_float_display(&body, round(a)); break;
        default:
            if (p >= 0) {
                snprintf(t, sizeof t, "%.*f", (int)p, a);
                lc_buf_puts(&body, t);
            } else {
                lc_buf_puts(&body, plain[0] == '-' ? plain + 1 : plain);
            }
        }
        if (f->grouping) {
            int64_t end = 0;
            while (end < body.len && body.p[end] >= '0' && body.p[end] <= '9') end++;
            lc_buf g = {0};
            group_digits(&g, body.p, end, f->grouping, 3);
            lc_buf_put(&g, body.p + end, body.len - end);
            free(body.p);
            body = g;
        }
    }
    if (upper)
        for (int64_t i = 0; i < body.len; i++)
            if (body.p[i] >= 'a' && body.p[i] <= 'z') body.p[i] -= 32;
    number(out, f, neg, "", body.p ? body.p : "");
    free(body.p);
}

static void fmt_int_spec(lc_buf *out, const lc_fmt *f, int64_t n) {
    char k = f->kind;
    bool float_kind = k == 'e' || k == 'E' || k == 'f' || k == 'F' || k == 'g' || k == 'G' || k == '%';
    if (float_kind || (f->precision >= 0 && !(k == 'b' || k == 'o' || k == 'x' || k == 'X' || k == 'c' || k == 'd' || k == 'n'))) {
        char plain[32];
        snprintf(plain, sizeof plain, "%lld", (long long)n);
        fmt_float_spec(out, f, (double)n, plain);
        return;
    }
    uint64_t m = n < 0 ? (uint64_t)0 - (uint64_t)n : (uint64_t)n;
    char d[80];
    const char *prefix = "";
    int group = 3;
    switch (k) {
    case 'x': snprintf(d, sizeof d, "%llx", (unsigned long long)m), prefix = "0x", group = 4; break;
    case 'X': snprintf(d, sizeof d, "%llX", (unsigned long long)m), prefix = "0X", group = 4; break;
    case 'o': snprintf(d, sizeof d, "%llo", (unsigned long long)m), prefix = "0o", group = 4; break;
    case 'b': {
        int len = 0;
        char tmp[80];
        do {
            tmp[len++] = (char)('0' + (m & 1));
            m >>= 1;
        } while (m);
        for (int i = 0; i < len; i++) d[i] = tmp[len - 1 - i];
        d[len] = 0;
        prefix = "0b";
        group = 4;
        break;
    }
    case 'c': {
        lc_buf c = {0};
        if (n >= 0 && n <= 0x10FFFF && !(n >= 0xD800 && n <= 0xDFFF)) put_fill(&c, (int)n, 1);
        text(out, f, c.p ? c.p : "", c.len);
        free(c.p);
        return;
    }
    default: snprintf(d, sizeof d, "%llu", (unsigned long long)m);
    }
    lc_buf g = {0};
    if (f->grouping)
        group_digits(&g, d, (int64_t)strlen(d), f->grouping, group);
    else
        lc_buf_puts(&g, d);
    number(out, f, n < 0, f->alt ? prefix : "", g.p);
    free(g.p);
}

void lc_buf_format(lc_buf *b, lc_v v, const lc_fmt *f, const char *site) {
    char k[64];
    if (v.tag == T_INT) {
        fmt_int_spec(b, f, v.u.i);
    } else if (v.tag == T_FLOAT) {
        lc_buf plain = {0};
        lc_buf_display(&plain, v);
        fmt_float_spec(b, f, v.u.f, plain.p);
        free(plain.p);
    } else if (f->kind && f->kind != 's') {
        lc_panic("E0301", site, "format `%c` needs a number, got %s", f->kind, lc_kind(v, k));
    } else if (v.tag == T_STR) {
        text(b, f, STR(v)->data, STR(v)->len);
    } else {
        lc_buf d = {0};
        lc_buf_display(&d, v);
        text(b, f, d.p ? d.p : "", d.len);
        free(d.p);
    }
}

/* ----- builtin functions ----- */

void lc_print(int argc, lc_v *args, bool err, const char *site) {
    lc_buf b = {0};
    lc_buf t = {0};
    for (int i = 0; i < argc; i++) {
        if (args[i].tag == T_ERR) lc_panic("E0406", site, "printing an error (%s); add `?`", disp(args[i], &t));
        if (i) lc_buf_putc(&b, ' ');
        lc_buf_display(&b, args[i]);
    }
    lc_buf_putc(&b, '\n');
    if (err) {
        lc_flush();
        fwrite(b.p, 1, b.len, stderr);
    } else {
        lc_out(b.p, b.len);
    }
    free(b.p);
}

lc_v lc_range_fn(int argc, lc_v *args, const char *site) {
    int64_t ns[3];
    for (int i = 0; i < argc; i++) ns[i] = lc_int_of(args[i], site);
    int64_t s = 0, e, st = 1;
    if (argc == 1) {
        e = ns[0];
    } else if (argc == 2) {
        s = ns[0], e = ns[1];
    } else if (argc == 3) {
        s = ns[0], e = ns[1], st = ns[2];
    } else {
        lc_panic("E0206", site, "range takes 1 to 3 arguments");
    }
    if (st == 0) lc_panic("E0301", site, "range step must not be 0");
    return lc_range_new(s, e, st);
}

lc_v lc_minmax(bool max, int argc, lc_v *args, const char *site) {
    char k1[64], k2[64];
    lc_v best = args[0];
    for (int i = 1; i < argc; i++) {
        bool ok;
        int c = lc_cmp(args[i], best, &ok);
        if (!ok) lc_panic("E0301", site, "cannot compare %s with %s", lc_kind(args[i], k1), lc_kind(best, k2));
        if ((!max && c < 0) || (max && c > 0)) best = args[i];
    }
    return lc_retain(best);
}

bool lc_heap_push(lc_v heap, lc_v x, const char *site);

lc_v lc_collect(int kind, int argc, lc_v *args, const char *site) {
    if (kind == 0) {
        lc_v s = lc_set_new();
        if (argc) {
            lc_v xs = lc_items(args[0], site);
            for (int64_t i = 0; i < VEC(xs)->len; i++) lc_set_add(s, lc_retain(lc_vget(VEC(xs), i)));
            lc_release(xs);
        }
        return s;
    }
    if (kind == 1) {
        lc_v h = lc_heap_new();
        if (argc) {
            lc_v xs = lc_items(args[0], site);
            for (int64_t i = 0; i < VEC(xs)->len; i++) lc_heap_push(h, lc_retain(lc_vget(VEC(xs), i)), site);
            lc_release(xs);
        }
        return h;
    }
    if (argc) return lc_items(args[0], site);
    return lc_list_new(0);
}

_Noreturn void lc_panic_fn(lc_v msg, const char *site) {
    lc_buf t = {0};
    lc_panic("E0400", site, "panic: %s", disp(msg, &t));
}

static const char *str_arg(int argc, lc_v *args, int i, const char *what, const char *site) {
    char k[64];
    if (i >= argc) lc_panic("E0206", site, "%s: missing argument", what);
    if (args[i].tag != T_STR) lc_panic("E0301", site, "%s: want str got %s", what, lc_kind(args[i], k));
    return STR(args[i])->data;
}

static lc_v io_err(const char *what, const char *path, int e) {
    lc_buf b = {0};
    lc_buf_puts(&b, "cannot ");
    lc_buf_puts(&b, what);
    lc_buf_putc(&b, ' ');
    lc_buf_puts(&b, path);
    lc_buf_puts(&b, ": ");
    if (e == ENOENT) {
        lc_buf_puts(&b, "no such file");
    } else if (e == EACCES || e == EPERM) {
        lc_buf_puts(&b, "permission denied");
    } else {
        char t[256];
        snprintf(t, sizeof t, "%s (os error %d)", strerror(e), e);
        lc_buf_puts(&b, t);
    }
    return lc_err_new(lc_buf_finish(&b));
}

static lc_v read_file(const char *path, bool *ok) {
    FILE *f = fopen(path, "rb");
    if (!f) {
        *ok = false;
        return io_err("read", path, errno);
    }
    lc_buf b = {0};
    char tmp[65536];
    size_t n;
    while ((n = fread(tmp, 1, sizeof tmp, f)) > 0) lc_buf_put(&b, tmp, (int64_t)n);
    int e = ferror(f) ? errno : 0;
    fclose(f);
    if (e) {
        free(b.p);
        *ok = false;
        return io_err("read", path, e);
    }
    *ok = true;
    if (!b.p) return lc_str_new("", 0);
    return lc_buf_finish(&b);
}

/* Rust's `str::lines`: split on `\n`, drop a `\r` before it and a final
 * empty line. */
lc_v lc_split_lines(lc_str *s) {
    lc_v out = lc_list_new(0);
    int64_t start = 0;
    for (int64_t i = 0; i < s->len; i++) {
        if (s->data[i] == '\n') {
            int64_t end = i;
            if (end > start && s->data[end - 1] == '\r') end--;
            lc_vec_push(out, lc_str_new(s->data + start, end - start));
            start = i + 1;
        }
    }
    if (start < s->len) {
        int64_t end = s->len;
        if (end > start && s->data[end - 1] == '\r') end--;
        lc_vec_push(out, lc_str_new(s->data + start, end - start));
    }
    return out;
}

lc_v lc_fs(int op, int argc, lc_v *args, const char *site) {
    static const char *names[] = {"fs.read", "fs.write", "fs.append", "fs.exists", "fs.lines", "fs.remove"};
    const char *path = str_arg(argc, args, 0, names[op], site);
    bool ok;
    switch (op) {
    case FS_READ: return read_file(path, &ok);
    case FS_LINES: {
        lc_v s = read_file(path, &ok);
        if (!ok) return s;
        lc_v out = lc_split_lines(STR(s));
        lc_release(s);
        return out;
    }
    case FS_WRITE:
    case FS_APPEND: {
        lc_buf b = {0};
        if (argc > 1) lc_buf_display(&b, args[1]);
        FILE *f = fopen(path, op == FS_WRITE ? "wb" : "ab");
        if (!f) {
            free(b.p);
            return io_err("write", path, errno);
        }
        size_t n = b.len ? fwrite(b.p, 1, b.len, f) : 0;
        int e = (n != (size_t)b.len) ? errno : 0;
        if (fclose(f) != 0 && !e) e = errno;
        free(b.p);
        if (e) return io_err("write", path, e);
        return LC_UNIT;
    }
    case FS_EXISTS: {
        struct stat st;
        return lc_bool(stat(path, &st) == 0);
    }
    default:
        if (remove(path) != 0) return io_err("remove", path, errno);
        return LC_UNIT;
    }
}

static lc_v read_stdin_all(void) {
    lc_flush();
    lc_buf b = {0};
    char tmp[65536];
    size_t n;
    while ((n = fread(tmp, 1, sizeof tmp, stdin)) > 0) lc_buf_put(&b, tmp, (int64_t)n);
    if (!b.p) return lc_str_new("", 0);
    return lc_buf_finish(&b);
}

lc_v lc_io(int op, int argc, lc_v *args, const char *site) {
    (void)site;
    switch (op) {
    case IO_READ: return read_stdin_all();
    case IO_LINES: {
        lc_v s = read_stdin_all();
        lc_v out = lc_split_lines(STR(s));
        lc_release(s);
        return out;
    }
    case IO_READ_LINE: {
        lc_flush();
        char *line = NULL;
        size_t cap = 0;
        ssize_t n = getline(&line, &cap, stdin);
        if (n <= 0) {
            free(line);
            return LC_NONE;
        }
        if (line[n - 1] == '\n') {
            n--;
            if (n > 0 && line[n - 1] == '\r') n--;
        }
        lc_v s = lc_str_new(line, n);
        free(line);
        return s;
    }
    default: {
        lc_buf b = {0};
        if (argc) lc_buf_display(&b, args[0]);
        if (b.len) lc_out(b.p, b.len);
        free(b.p);
        return LC_UNIT;
    }
    }
}

lc_v lc_os(int op, int argc, lc_v *args, const char *site) {
    switch (op) {
    case OS_ARGS: {
        lc_v out = lc_list_new(lc_argc);
        for (int i = 1; i < lc_argc; i++) lc_vec_push(out, lc_cstr(lc_argv[i]));
        return out;
    }
    case OS_ENV: {
        const char *v = getenv(str_arg(argc, args, 0, "os.env", site));
        return v ? lc_cstr(v) : LC_NONE;
    }
    case OS_EXIT: {
        int64_t code = lc_int_of(args[0], site);
        lc_exit((int)(code < 0 ? 0 : code > 255 ? 255 : code));
    }
    default: {
        struct timespec ts;
        clock_gettime(CLOCK_REALTIME, &ts);
        return lc_float((double)ts.tv_sec + ts.tv_nsec / 1e9);
    }
    }
}
