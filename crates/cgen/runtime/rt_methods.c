/* Builtin methods and in-place mutators. Mirrors crates/interp/src/builtins.rs. */
#include "lacon.h"

#include <math.h>
#include <stdio.h>
#include <stdlib.h>

typedef struct { lc_v key, val; } lc_kv;
bool lc_sort_kv(lc_kv *xs, int64_t n, char *a, char *b);
lc_v lc_heap_sorted(lc_v h);
lc_v lc_map_remove(lc_v map, lc_v k, bool *found);
void lc_map_clear(lc_v map);
lc_v lc_parse_number(lc_str *str, bool *ok);
lc_v lc_split_lines(lc_str *s);

const char *const lc_method_names[] = {
    "str", "to_string", "unwrap", "expect", "is_none", "is_some", "is_err", "is_ok", "clone", "iter", "into_iter", "collect",
    "to_owned", "copied", "cloned", "as_str", "abs", "pow", "min", "max", "clamp", "sign", "chr", "sqrt", "cbrt", "floor",
    "ceil", "round", "trunc", "fract", "exp", "ln", "log", "log2", "log10", "sin", "cos", "tan", "asin", "acos", "atan",
    "atan2", "hypot", "is_nan", "is_finite", "len", "is_empty", "chars", "bytes", "lines", "split", "split_once", "words",
    "trim", "trim_start", "trim_end", "starts_with", "ends_with", "contains", "contains_key", "find", "rfind", "replace",
    "upper", "lower", "repeat", "parse", "rev", "count", "is_digit", "is_alpha", "is_alnum", "is_space", "is_upper",
    "is_lower", "ord", "join", "strip_prefix", "strip_suffix", "pad_left", "pad_right", "capitalize", "first", "last", "get",
    "index", "map", "filter", "position", "any", "all", "sum", "product", "min_by", "max_by", "sort", "sort_by", "unique",
    "enumerate", "zip", "flat_map", "flatten", "take", "skip", "take_while", "skip_while", "chunks", "windows", "group_by",
    "partition", "fold", "reduce", "each", "to_list", "to_set", "to_map", "step_by", "keys", "values", "items", "union",
    "intersection", "difference", "is_subset", "peek", "push", "pop", "insert", "remove", "clear", "extend", "add", "swap",
    "truncate", "retain",
};

bool lc_is_mutator(int m) { return m >= M_push; }

/* ----- characters ----- */

uint32_t lc_utf8_decode(const char *s, int64_t n, int64_t *len) {
    unsigned char c = (unsigned char)s[0];
    if (c < 0x80 || n < 2) {
        *len = 1;
        return c;
    }
    if ((c & 0xE0) == 0xC0) {
        *len = 2;
        return ((c & 0x1F) << 6) | (s[1] & 0x3F);
    }
    if ((c & 0xF0) == 0xE0 && n >= 3) {
        *len = 3;
        return ((c & 0x0F) << 12) | ((s[1] & 0x3F) << 6) | (s[2] & 0x3F);
    }
    if (n >= 4) {
        *len = 4;
        return ((c & 0x07) << 18) | ((s[1] & 0x3F) << 12) | ((s[2] & 0x3F) << 6) | (s[3] & 0x3F);
    }
    *len = 1;
    return c;
}

static int utf8_encode(uint32_t cp, char *out) {
    if (cp < 0x80) {
        out[0] = (char)cp;
        return 1;
    }
    if (cp < 0x800) {
        out[0] = (char)(0xC0 | (cp >> 6));
        out[1] = (char)(0x80 | (cp & 0x3F));
        return 2;
    }
    if (cp < 0x10000) {
        out[0] = (char)(0xE0 | (cp >> 12));
        out[1] = (char)(0x80 | ((cp >> 6) & 0x3F));
        out[2] = (char)(0x80 | (cp & 0x3F));
        return 3;
    }
    out[0] = (char)(0xF0 | (cp >> 18));
    out[1] = (char)(0x80 | ((cp >> 12) & 0x3F));
    out[2] = (char)(0x80 | ((cp >> 6) & 0x3F));
    out[3] = (char)(0x80 | (cp & 0x3F));
    return 4;
}

/* Rust's `char::is_whitespace`. */
bool lc_is_ws(uint32_t c) {
    return (c >= 9 && c <= 13) || c == ' ' || c == 0x85 || c == 0xA0 || c == 0x1680 || (c >= 0x2000 && c <= 0x200A) || c == 0x2028 ||
           c == 0x2029 || c == 0x202F || c == 0x205F || c == 0x3000;
}

/* Approximations of Rust's Unicode classes: exact for ASCII and Latin-1. */
static bool is_upper_cp(uint32_t c) { return (c >= 'A' && c <= 'Z') || (c >= 0xC0 && c <= 0xDE && c != 0xD7); }
static bool is_lower_cp(uint32_t c) { return (c >= 'a' && c <= 'z') || (c >= 0xDF && c <= 0xFF && c != 0xF7); }
static bool is_alpha_cp(uint32_t c) {
    if (c < 0x80) return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z');
    if (c < 0x100) return c == 0xAA || c == 0xB5 || c == 0xBA || (c >= 0xC0 && c != 0xD7 && c != 0xF7);
    return !lc_is_ws(c) && !(c >= 0x2000 && c <= 0x2BFF) && !(c >= 0x3000 && c <= 0x303F);
}
static bool is_digit_cp(uint32_t c) { return c >= '0' && c <= '9'; }
static bool is_alnum_cp(uint32_t c) { return is_alpha_cp(c) || is_digit_cp(c); }
static uint32_t upper_cp(uint32_t c) { return ((c >= 'a' && c <= 'z') || (c >= 0xE0 && c <= 0xFE && c != 0xF7)) ? c - 32 : c; }
static uint32_t lower_cp(uint32_t c) { return ((c >= 'A' && c <= 'Z') || (c >= 0xC0 && c <= 0xDE && c != 0xD7)) ? c + 32 : c; }

void trim_ws(const char **s, int64_t *n) {
    const char *p = *s;
    int64_t len = *n, l;
    while (len > 0 && lc_is_ws(lc_utf8_decode(p, len, &l))) p += l, len -= l;
    while (len > 0) {
        int64_t i = len - 1;
        while (i > 0 && ((unsigned char)p[i] & 0xC0) == 0x80) i--;
        if (!lc_is_ws(lc_utf8_decode(p + i, len - i, &l))) break;
        len = i;
    }
    *s = p;
    *n = len;
}

/* ----- helpers ----- */

static const char *disp(lc_v v, lc_buf *b) {
    b->len = 0;
    lc_buf_display(b, v);
    return b->p ? b->p : "";
}

static lc_v arg(int argc, lc_v *args, int i, int m, const char *site) {
    if (i >= argc) lc_panic("E0206", site, "`%s` needs %d argument(s)", lc_method_names[m], i + 1);
    return args[i];
}

static int64_t int_arg(int argc, lc_v *args, int i, int m, const char *site) {
    char k[64];
    lc_v v = arg(argc, args, i, m, site);
    if (v.tag != T_INT) lc_panic("E0301", site, "`%s` wants an int argument, got %s", lc_method_names[m], lc_kind(v, k));
    return v.u.i;
}

static lc_str *sarg(int argc, lc_v *args, int i, int m, const char *site) {
    char k[64];
    lc_v v = arg(argc, args, i, m, site);
    if (v.tag != T_STR) lc_panic("E0301", site, "`%s` wants a str argument, got %s", lc_method_names[m], lc_kind(v, k));
    return STR(v);
}

static bool norm_index(int64_t n, int64_t len, int64_t *out) {
    int64_t i = n < 0 ? len + n : n;
    if (i < 0 || i >= len) return false;
    *out = i;
    return true;
}

static bool pred(lc_v f, lc_v x, const char *site, bool *stop) {
    char k[64];
    lc_v r = lc_call(f, 1, &x, site);
    if (lc_unwinding) {
        *stop = true;
        return false;
    }
    if (r.tag != T_BOOL) lc_panic("E0301", site, "predicate must return bool, got %s", lc_kind(r, k));
    return r.u.i != 0;
}

static lc_v str_from(const char *s, int64_t n) { return lc_str_new(s, n); }

static int64_t char_index(lc_str *s, int64_t byte) {
    int64_t c = 0;
    for (int64_t i = 0; i < byte; i++)
        if (((unsigned char)s->data[i] & 0xC0) != 0x80) c++;
    return c;
}

static const char *find_sub(const char *h, int64_t hn, const char *n, int64_t nn) {
    if (nn == 0) return h;
    return memmem(h, hn, n, nn);
}

static const char *rfind_sub(const char *h, int64_t hn, const char *n, int64_t nn) {
    if (nn > hn) return NULL;
    for (int64_t i = hn - nn; i >= 0; i--)
        if (memcmp(h + i, n, nn) == 0) return h + i;
    return NULL;
}

/* Splits on whitespace, dropping empty pieces. */
static lc_v split_ws(lc_str *s) {
    lc_v out = lc_list_new(0);
    int64_t i = 0, l;
    while (i < s->len) {
        while (i < s->len && lc_is_ws(lc_utf8_decode(s->data + i, s->len - i, &l))) i += l;
        if (i >= s->len) break;
        int64_t start = i;
        while (i < s->len && !lc_is_ws(lc_utf8_decode(s->data + i, s->len - i, &l))) i += l;
        lc_vec_push(out, str_from(s->data + start, i - start));
    }
    return out;
}

static lc_v chars_of(lc_str *s) {
    lc_v out = lc_list_new(s->nchars);
    int64_t i = 0, l;
    while (i < s->len) {
        lc_utf8_decode(s->data + i, s->len - i, &l);
        lc_vec_push(out, str_from(s->data + i, l));
        i += l;
    }
    return out;
}

/* ----- numbers ----- */

static double as_f64(lc_v v) { return v.tag == T_INT ? (double)v.u.i : v.u.f; }

static double num(int argc, lc_v *args, int i, int m, const char *site) {
    char k[64];
    lc_v v = arg(argc, args, i, m, site);
    if (v.tag == T_INT) return (double)v.u.i;
    if (v.tag == T_FLOAT) return v.u.f;
    lc_panic("E0301", site, "`%s` wants a number, got %s", lc_method_names[m], lc_kind(v, k));
}

static bool float_only(int m) {
    switch (m) {
    case M_sqrt: case M_cbrt: case M_exp: case M_ln: case M_log: case M_log2: case M_log10: case M_sin: case M_cos: case M_tan:
    case M_asin: case M_acos: case M_atan: case M_atan2: case M_hypot: case M_fract: case M_is_nan: case M_is_finite:
        return true;
    }
    return false;
}

static double signum(double x) {
    if (isnan(x)) return x;
    return signbit(x) ? -1.0 : 1.0;
}

static lc_v num_method(int m, lc_v recv, int argc, lc_v *args, const char *site) {
    char k[64];
    if (recv.tag == T_INT) {
        int64_t n = recv.u.i;
        switch (m) {
        case M_abs:
            if (n == INT64_MIN) lc_panic("E0405", site, "integer overflow in `abs`");
            return lc_int(n < 0 ? -n : n);
        case M_sign: return lc_int((n > 0) - (n < 0));
        case M_pow: return lc_binop(OP_POW, recv, arg(argc, args, 0, m, site), site);
        case M_chr: {
            if (n < 0 || n > 0x10FFFF || (n >= 0xD800 && n <= 0xDFFF)) lc_panic("E0301", site, "%lld is not a character code", (long long)n);
            char enc[4];
            return str_from(enc, utf8_encode((uint32_t)n, enc));
        }
        case M_floor:
        case M_ceil:
        case M_round:
        case M_trunc: return lc_int(n);
        case M_min:
        case M_max:
        case M_clamp: break;
        default:
            if (float_only(m)) return num_method(m, lc_float((double)n), argc, args, site);
            lc_panic("E0203", site, "int has no method `%s`", lc_method_names[m]);
        }
    }
    double x = as_f64(recv);
    switch (m) {
    case M_min:
    case M_max: {
        lc_v o = arg(argc, args, 0, m, site);
        bool ok;
        int c = lc_cmp(recv, o, &ok);
        if (!ok) lc_panic("E0301", site, "`%s` wants a number, got %s", lc_method_names[m], lc_kind(o, k));
        bool pick_recv = m == M_min ? c <= 0 : c >= 0;
        lc_v v = pick_recv ? recv : o;
        if (recv.tag == T_INT && o.tag == T_INT) return v;
        return lc_float(as_f64(v));
    }
    case M_clamp: {
        lc_v lo = arg(argc, args, 0, m, site), hi = arg(argc, args, 1, m, site);
        bool ok1, ok2;
        lc_v v = recv;
        if (lc_cmp(recv, lo, &ok1) < 0 && ok1)
            v = lo;
        else if (lc_cmp(recv, hi, &ok2) > 0 && ok2)
            v = hi;
        if (recv.tag == T_INT && lo.tag == T_INT && hi.tag == T_INT) return v;
        return lc_float(as_f64(v));
    }
    case M_abs: return lc_float(fabs(x));
    case M_sign: return lc_float(signum(x));
    case M_sqrt: return lc_float(sqrt(x));
    case M_cbrt: return lc_float(cbrt(x));
    case M_floor: return lc_float(floor(x));
    case M_ceil: return lc_float(ceil(x));
    case M_round:
        if (argc > 0) {
            int64_t d = lc_int_of(args[0], site);
            double mult = pow(10.0, (double)(int32_t)d);
            return lc_float(round(x * mult) / mult);
        }
        return lc_float(round(x));
    case M_trunc: return lc_float(trunc(x));
    case M_fract: return lc_float(x - trunc(x));
    case M_pow: return lc_float(pow(x, num(argc, args, 0, m, site)));
    case M_exp: return lc_float(exp(x));
    case M_ln: return lc_float(log(x));
    case M_log:
        if (argc > 0) return lc_float(log(x) / log(num(argc, args, 0, m, site)));
        return lc_float(log(x));
    case M_log2: return lc_float(log2(x));
    case M_log10: return lc_float(log10(x));
    case M_sin: return lc_float(sin(x));
    case M_cos: return lc_float(cos(x));
    case M_tan: return lc_float(tan(x));
    case M_asin: return lc_float(asin(x));
    case M_acos: return lc_float(acos(x));
    case M_atan: return lc_float(atan(x));
    case M_atan2: return lc_float(atan2(x, num(argc, args, 0, m, site)));
    case M_hypot: return lc_float(hypot(x, num(argc, args, 0, m, site)));
    case M_is_nan: return lc_bool(isnan(x));
    case M_is_finite: return lc_bool(isfinite(x));
    }
    lc_panic("E0203", site, "%s has no method `%s`", lc_kind(recv, k), lc_method_names[m]);
}

/* ----- strings ----- */

static bool char_class(lc_str *s, bool (*f)(uint32_t)) {
    if (s->len == 0) return false;
    int64_t i = 0, l;
    while (i < s->len) {
        if (!f(lc_utf8_decode(s->data + i, s->len - i, &l))) return false;
        i += l;
    }
    return true;
}

static lc_v map_chars(lc_str *s, uint32_t (*f)(uint32_t)) {
    lc_buf b = {0};
    int64_t i = 0, l;
    char enc[4];
    while (i < s->len) {
        uint32_t c = lc_utf8_decode(s->data + i, s->len - i, &l);
        lc_buf_put(&b, enc, utf8_encode(f(c), enc));
        i += l;
    }
    if (!b.p) return str_from("", 0);
    return lc_buf_finish(&b);
}

static int cmp_u32(const void *a, const void *b) {
    uint32_t x = *(const uint32_t *)a, y = *(const uint32_t *)b;
    return (x > y) - (x < y);
}

/* Returns true and sets *out when the method is a string method. */
static bool str_method(int m, lc_str *s, int argc, lc_v *args, const char *site, lc_v *out) {
    int64_t l;
    switch (m) {
    case M_len: *out = lc_int(s->nchars); return true;
    case M_is_empty: *out = lc_bool(s->len == 0); return true;
    case M_chars: *out = chars_of(s); return true;
    case M_bytes: {
        lc_v o = lc_list_new(s->len);
        for (int64_t i = 0; i < s->len; i++) lc_vec_push(o, lc_int((unsigned char)s->data[i]));
        *out = o;
        return true;
    }
    case M_lines: *out = lc_split_lines(s); return true;
    case M_words: *out = split_ws(s); return true;
    case M_split: {
        if (argc == 0) {
            *out = split_ws(s);
            return true;
        }
        lc_str *sep = sarg(argc, args, 0, m, site);
        if (sep->len == 0) {
            *out = chars_of(s);
            return true;
        }
        lc_v o = lc_list_new(0);
        const char *p = s->data, *end = s->data + s->len;
        for (;;) {
            const char *q = find_sub(p, end - p, sep->data, sep->len);
            if (!q) {
                lc_vec_push(o, str_from(p, end - p));
                break;
            }
            lc_vec_push(o, str_from(p, q - p));
            p = q + sep->len;
        }
        *out = o;
        return true;
    }
    case M_split_once: {
        lc_str *sep = sarg(argc, args, 0, m, site);
        const char *q = find_sub(s->data, s->len, sep->data, sep->len);
        if (!q) {
            *out = LC_NONE;
            return true;
        }
        lc_v pair[2] = {str_from(s->data, q - s->data), str_from(q + sep->len, s->data + s->len - q - sep->len)};
        *out = lc_tuple_of(2, pair);
        return true;
    }
    case M_trim:
    case M_trim_start:
    case M_trim_end: {
        const char *p = s->data;
        int64_t n = s->len;
        if (argc > 0) {
            /* The characters to remove, as in Python's `strip`. */
            lc_str *set = sarg(argc, args, 0, m, site);
            if (m != M_trim_end) {
                while (n > 0) {
                    lc_utf8_decode(p, n, &l);
                    if (!find_sub(set->data, set->len, p, l)) break;
                    p += l, n -= l;
                }
            }
            if (m != M_trim_start) {
                while (n > 0) {
                    int64_t i = n - 1;
                    while (i > 0 && ((unsigned char)p[i] & 0xC0) == 0x80) i--;
                    if (!find_sub(set->data, set->len, p + i, n - i)) break;
                    n = i;
                }
            }
        } else if (m == M_trim) {
            trim_ws(&p, &n);
        } else if (m == M_trim_start) {
            while (n > 0 && lc_is_ws(lc_utf8_decode(p, n, &l))) p += l, n -= l;
        } else {
            while (n > 0) {
                int64_t i = n - 1;
                while (i > 0 && ((unsigned char)p[i] & 0xC0) == 0x80) i--;
                if (!lc_is_ws(lc_utf8_decode(p + i, n - i, &l))) break;
                n = i;
            }
        }
        *out = str_from(p, n);
        return true;
    }
    case M_starts_with: {
        lc_str *x = sarg(argc, args, 0, m, site);
        *out = lc_bool(x->len <= s->len && memcmp(s->data, x->data, x->len) == 0);
        return true;
    }
    case M_ends_with: {
        lc_str *x = sarg(argc, args, 0, m, site);
        *out = lc_bool(x->len <= s->len && memcmp(s->data + s->len - x->len, x->data, x->len) == 0);
        return true;
    }
    case M_contains: {
        lc_str *x = sarg(argc, args, 0, m, site);
        *out = lc_bool(find_sub(s->data, s->len, x->data, x->len) != NULL);
        return true;
    }
    case M_find:
    case M_index: {
        if (argc > 0 && args[0].tag == T_FUNC) return false;
        lc_str *x = sarg(argc, args, 0, m, site);
        const char *q = find_sub(s->data, s->len, x->data, x->len);
        *out = q ? lc_int(char_index(s, q - s->data)) : LC_NONE;
        return true;
    }
    case M_rfind: {
        lc_str *x = sarg(argc, args, 0, m, site);
        const char *q = rfind_sub(s->data, s->len, x->data, x->len);
        *out = q ? lc_int(char_index(s, q - s->data)) : LC_NONE;
        return true;
    }
    case M_replace: {
        lc_str *a = sarg(argc, args, 0, m, site), *b = sarg(argc, args, 1, m, site);
        lc_buf o = {0};
        if (a->len == 0) {
            int64_t i = 0;
            lc_buf_put(&o, b->data, b->len);
            while (i < s->len) {
                lc_utf8_decode(s->data + i, s->len - i, &l);
                lc_buf_put(&o, s->data + i, l);
                lc_buf_put(&o, b->data, b->len);
                i += l;
            }
        } else {
            const char *p = s->data, *end = s->data + s->len;
            for (;;) {
                const char *q = find_sub(p, end - p, a->data, a->len);
                if (!q) {
                    lc_buf_put(&o, p, end - p);
                    break;
                }
                lc_buf_put(&o, p, q - p);
                lc_buf_put(&o, b->data, b->len);
                p = q + a->len;
            }
        }
        *out = o.p ? lc_buf_finish(&o) : str_from("", 0);
        return true;
    }
    case M_upper: *out = map_chars(s, upper_cp); return true;
    case M_lower: *out = map_chars(s, lower_cp); return true;
    case M_capitalize: {
        if (s->len == 0) {
            *out = str_from("", 0);
            return true;
        }
        uint32_t c = lc_utf8_decode(s->data, s->len, &l);
        char enc[4];
        lc_buf o = {0};
        lc_buf_put(&o, enc, utf8_encode(upper_cp(c), enc));
        lc_buf_put(&o, s->data + l, s->len - l);
        *out = lc_buf_finish(&o);
        return true;
    }
    case M_repeat: {
        int64_t n = int_arg(argc, args, 0, m, site);
        lc_buf o = {0};
        for (int64_t i = 0; i < n; i++) lc_buf_put(&o, s->data, s->len);
        *out = o.p ? lc_buf_finish(&o) : str_from("", 0);
        return true;
    }
    case M_parse: {
        bool ok;
        lc_v v = lc_parse_number(s, &ok);
        if (ok) {
            *out = v;
            return true;
        }
        const char *p = s->data;
        int64_t n = s->len;
        trim_ws(&p, &n);
        if (n == 4 && memcmp(p, "true", 4) == 0) {
            *out = LC_TRUE;
        } else if (n == 5 && memcmp(p, "false", 5) == 0) {
            *out = LC_FALSE;
        } else {
            lc_buf o = {0};
            lc_buf_puts(&o, "cannot parse ");
            lc_buf_repr(&o, lc_obj_v(T_STR, s));
            lc_buf_puts(&o, " as a number");
            *out = lc_err_new(lc_buf_finish(&o));
        }
        return true;
    }
    case M_rev: {
        lc_buf o = {0};
        int64_t i = s->len;
        while (i > 0) {
            int64_t j = i - 1;
            while (j > 0 && ((unsigned char)s->data[j] & 0xC0) == 0x80) j--;
            lc_buf_put(&o, s->data + j, i - j);
            i = j;
        }
        *out = o.p ? lc_buf_finish(&o) : str_from("", 0);
        return true;
    }
    case M_sort: {
        uint32_t *cs = malloc(sizeof(uint32_t) * (s->nchars + 1));
        int64_t n = 0, i = 0;
        while (i < s->len) {
            cs[n++] = lc_utf8_decode(s->data + i, s->len - i, &l);
            i += l;
        }
        qsort(cs, n, sizeof(uint32_t), cmp_u32);
        lc_buf o = {0};
        char enc[4];
        for (int64_t j = 0; j < n; j++) lc_buf_put(&o, enc, utf8_encode(cs[j], enc));
        free(cs);
        *out = o.p ? lc_buf_finish(&o) : str_from("", 0);
        return true;
    }
    case M_count: {
        if (argc > 0 && args[0].tag == T_STR) {
            lc_str *sub = STR(args[0]);
            if (sub->len == 0) {
                *out = lc_int(s->nchars + 1);
                return true;
            }
            int64_t c = 0;
            const char *p = s->data, *end = s->data + s->len;
            for (;;) {
                const char *q = find_sub(p, end - p, sub->data, sub->len);
                if (!q) break;
                c++;
                p = q + sub->len;
            }
            *out = lc_int(c);
            return true;
        }
        return false;
    }
    case M_is_digit: *out = lc_bool(char_class(s, is_digit_cp)); return true;
    case M_is_alpha: *out = lc_bool(char_class(s, is_alpha_cp)); return true;
    case M_is_alnum: *out = lc_bool(char_class(s, is_alnum_cp)); return true;
    case M_is_space: *out = lc_bool(char_class(s, lc_is_ws)); return true;
    case M_is_upper:
    case M_is_lower: {
        bool any_alpha = false, any_other = false;
        int64_t i = 0;
        while (i < s->len) {
            uint32_t c = lc_utf8_decode(s->data + i, s->len - i, &l);
            if (is_alpha_cp(c)) any_alpha = true;
            if (m == M_is_upper ? is_lower_cp(c) : is_upper_cp(c)) any_other = true;
            i += l;
        }
        *out = lc_bool(any_alpha && !any_other);
        return true;
    }
    case M_ord:
        if (s->len == 0) lc_panic("E0301", site, "`ord` on an empty string");
        *out = lc_int(lc_utf8_decode(s->data, s->len, &l));
        return true;
    case M_join: {
        lc_v xs = lc_items(arg(argc, args, 0, m, site), site);
        lc_buf o = {0};
        for (int64_t i = 0; i < VEC(xs)->len; i++) {
            if (i) lc_buf_put(&o, s->data, s->len);
            lc_buf_display(&o, VEC(xs)->items[i]);
        }
        lc_release(xs);
        *out = o.p ? lc_buf_finish(&o) : str_from("", 0);
        return true;
    }
    case M_strip_prefix: {
        lc_str *x = sarg(argc, args, 0, m, site);
        *out = (x->len <= s->len && memcmp(s->data, x->data, x->len) == 0) ? str_from(s->data + x->len, s->len - x->len) : LC_NONE;
        return true;
    }
    case M_strip_suffix: {
        lc_str *x = sarg(argc, args, 0, m, site);
        *out = (x->len <= s->len && memcmp(s->data + s->len - x->len, x->data, x->len) == 0) ? str_from(s->data, s->len - x->len) : LC_NONE;
        return true;
    }
    case M_pad_left:
    case M_pad_right: {
        int64_t w = int_arg(argc, args, 0, m, site);
        if (w < 0) w = 0;
        const char *fill = " ";
        int64_t fl = 1;
        if (argc > 1 && args[1].tag == T_STR && STR(args[1])->len > 0) {
            lc_utf8_decode(STR(args[1])->data, STR(args[1])->len, &fl);
            fill = STR(args[1])->data;
        }
        lc_buf o = {0};
        int64_t padn = w > s->nchars ? w - s->nchars : 0;
        if (m == M_pad_right) lc_buf_put(&o, s->data, s->len);
        for (int64_t i = 0; i < padn; i++) lc_buf_put(&o, fill, fl);
        if (m == M_pad_left) lc_buf_put(&o, s->data, s->len);
        *out = o.p ? lc_buf_finish(&o) : str_from("", 0);
        return true;
    }
    case M_get: {
        int64_t i = int_arg(argc, args, 0, m, site), idx;
        if (!norm_index(i, s->nchars, &idx)) {
            *out = LC_NONE;
            return true;
        }
        lc_v cs = chars_of(s);
        *out = lc_retain(VEC(cs)->items[idx]);
        lc_release(cs);
        return true;
    }
    case M_first:
    case M_last: {
        if (s->len == 0) {
            *out = LC_NONE;
            return true;
        }
        if (m == M_first) {
            lc_utf8_decode(s->data, s->len, &l);
            *out = str_from(s->data, l);
        } else {
            int64_t j = s->len - 1;
            while (j > 0 && ((unsigned char)s->data[j] & 0xC0) == 0x80) j--;
            *out = str_from(s->data + j, s->len - j);
        }
        return true;
    }
    }
    return false;
}

/* ----- heaps ----- */

static char g_ka[64], g_kb[64];

static bool heap_less(lc_v a, lc_v b, bool *bad) {
    bool ok;
    int c = lc_cmp(a, b, &ok);
    if (!ok) {
        *bad = true;
        lc_kind(a, g_ka);
        lc_kind(b, g_kb);
        return false;
    }
    return c < 0;
}

static const char *heap_kind(lc_v v, char *buf) {
    switch (v.tag) {
    case T_INT: return "int";
    case T_FLOAT: return "f64";
    case T_STR: return "str";
    case T_TUPLE: sprintf(buf, "%lld-tuple", (long long)VEC(v)->len); return buf;
    case T_NONE: return "none";
    }
    return "value";
}

bool lc_heap_push(lc_v heap, lc_v x, const char *site) {
    lc_vec *h = VEC(heap);
    lc_vec_push(heap, x);
    int64_t i = h->len - 1;
    while (i > 0) {
        int64_t parent = (i - 1) / 2;
        bool bad = false;
        bool lt = heap_less(h->items[i], h->items[parent], &bad);
        if (bad) {
            char a[64], b[64];
            lc_v item = h->items[i];
            const char *ka = heap_kind(item, a), *kb = heap_kind(h->items[parent], b);
            lc_panic("E0301", site, "heap items must be comparable, got %s and %s", ka, kb);
        }
        if (!lt) break;
        lc_v t = h->items[i];
        h->items[i] = h->items[parent];
        h->items[parent] = t;
        i = parent;
    }
    return true;
}

static lc_v heap_pop(lc_v heap) {
    lc_vec *h = VEC(heap);
    if (h->len == 0) return LC_NONE;
    lc_v top = h->items[0];
    h->items[0] = h->items[h->len - 1];
    h->len--;
    int64_t i = 0;
    for (;;) {
        int64_t l = 2 * i + 1, r = 2 * i + 2, m = i;
        bool bad = false;
        if (l < h->len && heap_less(h->items[l], h->items[m], &bad)) m = l;
        if (r < h->len && heap_less(h->items[r], h->items[m], &bad)) m = r;
        if (m == i) break;
        lc_v t = h->items[i];
        h->items[i] = h->items[m];
        h->items[m] = t;
        i = m;
    }
    return top;
}

/* ----- methods on any iterable ----- */

static void flatten_into(lc_v out, lc_v v) {
    switch (v.tag) {
    case T_LIST:
    case T_TUPLE:
        for (int64_t i = 0; i < VEC(v)->len; i++) lc_vec_push(out, lc_retain(VEC(v)->items[i]));
        break;
    case T_SET:
        for (int64_t i = 0; i < MAP(v)->len; i++) lc_vec_push(out, lc_retain(MAP(v)->e[i].k));
        break;
    case T_HEAP: {
        lc_v s = lc_heap_sorted(v);
        for (int64_t i = 0; i < VEC(s)->len; i++) lc_vec_push(out, lc_retain(VEC(s)->items[i]));
        lc_release(s);
        break;
    }
    case T_RANGE: {
        lc_range *r = RANGE(v);
        int64_t n = lc_range_len(r);
        for (int64_t i = 0; i < n; i++) lc_vec_push(out, lc_int(r->start + r->step * i));
        break;
    }
    case T_NONE: break;
    default: lc_vec_push(out, lc_retain(v));
    }
}

static lc_v ext_by(lc_v xs, lc_v *f, int want, const char *site) {
    char k1[64], k2[64];
    lc_v best = LC_UNIT, bk = LC_UNIT;
    bool have = false;
    lc_vec *v = VEC(xs);
    for (int64_t i = 0; i < v->len; i++) {
        lc_v x = v->items[i];
        lc_v key = f ? lc_call(*f, 1, &x, site) : lc_retain(x);
        if (lc_unwinding) {
            if (have) lc_release(bk);
            return LC_UNIT;
        }
        bool better;
        if (!have) {
            better = true;
        } else {
            bool ok;
            int c = lc_cmp(key, bk, &ok);
            if (!ok) lc_panic("E0301", site, "cannot compare %s with %s", lc_kind(key, k1), lc_kind(bk, k2));
            better = c == want;
        }
        if (better) {
            if (have) lc_release(bk);
            bk = key;
            best = x;
            have = true;
        } else {
            lc_release(key);
        }
    }
    if (!have) return LC_NONE;
    lc_release(bk);
    return lc_retain(best);
}

#define CALL1(f, x) ({ lc_v _a = (x); lc_v _r = lc_call((f), 1, &_a, site); if (lc_unwinding) { lc_release(xs); lc_release(out); return LC_UNIT; } _r; })

static lc_v iter_method(int m, lc_v recv, int argc, lc_v *args, const char *site) {
    char k[64];
    switch (recv.tag) {
    case T_LIST: case T_TUPLE: case T_SET: case T_HEAP: case T_MAP: case T_RANGE: case T_STR: break;
    default: lc_panic("E0203", site, "%s has no method `%s`", lc_kind(recv, k), lc_method_names[m]);
    }
    lc_v xs = lc_items(recv, site);
    lc_vec *v = VEC(xs);
    lc_v out = LC_UNIT;
    switch (m) {
    case M_len: out = lc_int(v->len); break;
    case M_is_empty: out = lc_bool(v->len == 0); break;
    case M_first: out = v->len ? lc_retain(v->items[0]) : LC_NONE; break;
    case M_last: out = v->len ? lc_retain(v->items[v->len - 1]) : LC_NONE; break;
    case M_get: {
        int64_t i = int_arg(argc, args, 0, m, site), idx;
        out = norm_index(i, v->len, &idx) ? lc_retain(v->items[idx]) : LC_NONE;
        break;
    }
    case M_contains: {
        bool found = false;
        for (int64_t i = 0; i < v->len && !found; i++) found = lc_eq(v->items[i], arg(argc, args, 0, m, site)) == 1;
        out = lc_bool(found);
        break;
    }
    case M_index: {
        lc_v x = arg(argc, args, 0, m, site);
        out = LC_NONE;
        for (int64_t i = 0; i < v->len; i++)
            if (lc_eq(v->items[i], x) == 1) {
                out = lc_int(i);
                break;
            }
        break;
    }
    case M_to_list: out = lc_retain(xs); break;
    case M_to_set: {
        out = lc_set_new();
        for (int64_t i = 0; i < v->len; i++) lc_set_add(out, lc_retain(v->items[i]));
        break;
    }
    case M_to_map: {
        out = lc_map_new();
        for (int64_t i = 0; i < v->len; i++) {
            lc_v x = v->items[i];
            if ((x.tag == T_TUPLE || x.tag == T_LIST) && VEC(x)->len == 2) {
                lc_map_put(out, lc_retain(VEC(x)->items[0]), lc_retain(VEC(x)->items[1]));
            } else {
                lc_v s = lc_short(x);
                lc_panic("E0301", site, "`to_map` needs (key, value) pairs, got %s", STR(s)->data);
            }
        }
        break;
    }
    case M_map: {
        lc_v f = arg(argc, args, 0, m, site);
        out = lc_list_new(v->len);
        for (int64_t i = 0; i < v->len; i++) lc_vec_push(out, CALL1(f, v->items[i]));
        break;
    }
    case M_filter:
    case M_find:
    case M_position:
    case M_take_while:
    case M_skip_while:
    case M_partition: {
        lc_v f = arg(argc, args, 0, m, site);
        lc_v no = LC_UNIT;
        if (m == M_filter) out = lc_list_new(0);
        if (m == M_find || m == M_position) out = LC_NONE;
        if (m == M_partition) {
            out = lc_list_new(0);
            no = lc_list_new(0);
        }
        int64_t i = 0;
        for (; i < v->len; i++) {
            bool stop = false;
            bool p = pred(f, v->items[i], site, &stop);
            if (stop) {
                lc_release(xs);
                lc_release(out);
                lc_release(no);
                return LC_UNIT;
            }
            if (m == M_filter && p) lc_vec_push(out, lc_retain(v->items[i]));
            if (m == M_find && p) {
                out = lc_retain(v->items[i]);
                break;
            }
            if (m == M_position && p) {
                out = lc_int(i);
                break;
            }
            if (m == M_partition) lc_vec_push(p ? out : no, lc_retain(v->items[i]));
            if ((m == M_take_while || m == M_skip_while) && !p) break;
        }
        if (m == M_take_while || m == M_skip_while) {
            int64_t a = m == M_take_while ? 0 : i, b = m == M_take_while ? i : v->len;
            out = lc_list_new(b - a);
            for (int64_t j = a; j < b; j++) lc_vec_push(out, lc_retain(v->items[j]));
        }
        if (m == M_partition) {
            lc_v pair[2] = {out, no};
            out = lc_tuple_of(2, pair);
        }
        break;
    }
    case M_any:
    case M_all: {
        bool want = m == M_any;
        out = lc_bool(!want);
        for (int64_t i = 0; i < v->len; i++) {
            bool b;
            if (argc > 0) {
                bool stop = false;
                b = pred(args[0], v->items[i], site, &stop);
                if (stop) {
                    lc_release(xs);
                    return LC_UNIT;
                }
            } else {
                lc_v x = v->items[i];
                if (x.tag != T_BOOL) lc_panic("E0301", site, "`%s()` without a predicate needs bools, got %s", lc_method_names[m], lc_kind(x, k));
                b = x.u.i != 0;
            }
            if (b == want) {
                out = lc_bool(want);
                break;
            }
        }
        break;
    }
    case M_count: {
        int64_t c = 0;
        if (argc == 0) {
            c = v->len;
        } else if (args[0].tag == T_FUNC) {
            for (int64_t i = 0; i < v->len; i++) {
                bool stop = false;
                if (pred(args[0], v->items[i], site, &stop)) c++;
                if (stop) {
                    lc_release(xs);
                    return LC_UNIT;
                }
            }
        } else {
            for (int64_t i = 0; i < v->len; i++)
                if (lc_eq(v->items[i], args[0]) == 1) c++;
        }
        out = lc_int(c);
        break;
    }
    case M_sum:
    case M_product: {
        lc_v acc = lc_int(m == M_sum ? 0 : 1);
        for (int64_t i = 0; i < v->len; i++) {
            lc_v next = lc_binop(m == M_sum ? OP_ADD : OP_MUL, acc, v->items[i], site);
            lc_release(acc);
            acc = next;
        }
        out = acc;
        break;
    }
    case M_min:
    case M_max:
        if (argc == 0) {
            out = ext_by(xs, NULL, m == M_min ? -1 : 1, site);
            break;
        }
        lc_panic("E0203", site, "%s has no method `%s`", lc_kind(recv, k), lc_method_names[m]);
    case M_min_by:
    case M_max_by: {
        lc_v f = arg(argc, args, 0, m, site);
        out = ext_by(xs, &f, m == M_min_by ? -1 : 1, site);
        if (lc_unwinding) {
            lc_release(xs);
            return LC_UNIT;
        }
        break;
    }
    case M_sort:
    case M_sort_by: {
        lc_kv *kv = malloc(sizeof(lc_kv) * (v->len ? v->len : 1));
        lc_v f = m == M_sort_by ? arg(argc, args, 0, m, site) : LC_UNIT;
        for (int64_t i = 0; i < v->len; i++) {
            lc_v key = m == M_sort_by ? lc_call(f, 1, &v->items[i], site) : lc_retain(v->items[i]);
            if (lc_unwinding) {
                for (int64_t j = 0; j < i; j++) lc_release(kv[j].key);
                free(kv);
                lc_release(xs);
                return LC_UNIT;
            }
            kv[i] = (lc_kv){key, v->items[i]};
        }
        char a[64], b[64];
        if (!lc_sort_kv(kv, v->len, a, b)) {
            if (m == M_sort) lc_panic("E0301", site, "cannot sort: %s and %s are not comparable", a, b);
            lc_panic("E0301", site, "cannot sort: keys %s and %s are not comparable", a, b);
        }
        out = lc_list_new(v->len);
        for (int64_t i = 0; i < v->len; i++) {
            lc_vec_push(out, lc_retain(kv[i].val));
            lc_release(kv[i].key);
        }
        free(kv);
        break;
    }
    case M_rev: {
        out = lc_list_new(v->len);
        for (int64_t i = v->len - 1; i >= 0; i--) lc_vec_push(out, lc_retain(v->items[i]));
        break;
    }
    case M_unique: {
        lc_v s = lc_set_new();
        for (int64_t i = 0; i < v->len; i++) lc_set_add(s, lc_retain(v->items[i]));
        out = lc_list_new(MAP(s)->len);
        for (int64_t i = 0; i < MAP(s)->len; i++) lc_vec_push(out, lc_retain(MAP(s)->e[i].k));
        lc_release(s);
        break;
    }
    case M_enumerate: {
        int64_t start = argc > 0 ? lc_int_of(args[0], site) : 0;
        out = lc_list_new(v->len);
        for (int64_t i = 0; i < v->len; i++) {
            lc_v pair[2] = {lc_int(start + i), lc_retain(v->items[i])};
            lc_vec_push(out, lc_tuple_of(2, pair));
        }
        break;
    }
    case M_zip: {
        lc_v ys = lc_items(arg(argc, args, 0, m, site), site);
        int64_t n = v->len < VEC(ys)->len ? v->len : VEC(ys)->len;
        out = lc_list_new(n);
        for (int64_t i = 0; i < n; i++) {
            lc_v pair[2] = {lc_retain(v->items[i]), lc_retain(VEC(ys)->items[i])};
            lc_vec_push(out, lc_tuple_of(2, pair));
        }
        lc_release(ys);
        break;
    }
    case M_flat_map: {
        lc_v f = arg(argc, args, 0, m, site);
        out = lc_list_new(0);
        for (int64_t i = 0; i < v->len; i++) {
            lc_v r = CALL1(f, v->items[i]);
            flatten_into(out, r);
            lc_release(r);
        }
        break;
    }
    case M_flatten: {
        out = lc_list_new(0);
        for (int64_t i = 0; i < v->len; i++) flatten_into(out, v->items[i]);
        break;
    }
    case M_take:
    case M_skip: {
        int64_t n = int_arg(argc, args, 0, m, site);
        if (n < 0) n = 0;
        int64_t a = m == M_take ? 0 : (n < v->len ? n : v->len), b = m == M_take ? (n < v->len ? n : v->len) : v->len;
        out = lc_list_new(b - a);
        for (int64_t i = a; i < b; i++) lc_vec_push(out, lc_retain(v->items[i]));
        break;
    }
    case M_step_by: {
        int64_t n = int_arg(argc, args, 0, m, site);
        if (n <= 0) lc_panic("E0301", site, "step_by needs a positive step");
        out = lc_list_new(0);
        for (int64_t i = 0; i < v->len; i += n) lc_vec_push(out, lc_retain(v->items[i]));
        break;
    }
    case M_chunks:
    case M_windows: {
        int64_t n = int_arg(argc, args, 0, m, site);
        if (n <= 0) lc_panic("E0301", site, "`%s` needs a positive size", lc_method_names[m]);
        out = lc_list_new(0);
        if (m == M_chunks) {
            for (int64_t i = 0; i < v->len; i += n) {
                int64_t e = i + n < v->len ? i + n : v->len;
                lc_v c = lc_list_new(e - i);
                for (int64_t j = i; j < e; j++) lc_vec_push(c, lc_retain(v->items[j]));
                lc_vec_push(out, c);
            }
        } else {
            for (int64_t i = 0; i + n <= v->len; i++) {
                lc_v c = lc_list_new(n);
                for (int64_t j = i; j < i + n; j++) lc_vec_push(c, lc_retain(v->items[j]));
                lc_vec_push(out, c);
            }
        }
        break;
    }
    case M_join: {
        lc_buf o = {0};
        const char *sep = "";
        int64_t sl = 0;
        if (argc > 0) {
            if (args[0].tag != T_STR) lc_panic("E0301", site, "`join` wants a str separator, got %s", lc_kind(args[0], k));
            sep = STR(args[0])->data;
            sl = STR(args[0])->len;
        }
        for (int64_t i = 0; i < v->len; i++) {
            if (i) lc_buf_put(&o, sep, sl);
            lc_buf_display(&o, v->items[i]);
        }
        out = o.p ? lc_buf_finish(&o) : lc_str_new("", 0);
        break;
    }
    case M_group_by: {
        lc_v f = arg(argc, args, 0, m, site);
        out = lc_map_new();
        for (int64_t i = 0; i < v->len; i++) {
            lc_v key = CALL1(f, v->items[i]);
            lc_entry *e = lc_map_find(MAP(out), key);
            if (e) {
                lc_vec_push(e->v, lc_retain(v->items[i]));
                lc_release(key);
            } else {
                lc_v l = lc_list_new(1);
                lc_vec_push(l, lc_retain(v->items[i]));
                lc_map_put(out, key, l);
            }
        }
        break;
    }
    case M_fold: {
        lc_v acc = lc_retain(arg(argc, args, 0, m, site));
        lc_v f = arg(argc, args, 1, m, site);
        for (int64_t i = 0; i < v->len; i++) {
            lc_v two[2] = {acc, v->items[i]};
            lc_v next = lc_call(f, 2, two, site);
            lc_release(acc);
            if (lc_unwinding) {
                lc_release(xs);
                return LC_UNIT;
            }
            acc = next;
        }
        out = acc;
        break;
    }
    case M_reduce: {
        lc_v f = arg(argc, args, 0, m, site);
        if (v->len == 0) {
            out = LC_NONE;
            break;
        }
        lc_v acc = lc_retain(v->items[0]);
        for (int64_t i = 1; i < v->len; i++) {
            lc_v two[2] = {acc, v->items[i]};
            lc_v next = lc_call(f, 2, two, site);
            lc_release(acc);
            if (lc_unwinding) {
                lc_release(xs);
                return LC_UNIT;
            }
            acc = next;
        }
        out = acc;
        break;
    }
    case M_each: {
        lc_v f = arg(argc, args, 0, m, site);
        for (int64_t i = 0; i < v->len; i++) lc_release(CALL1(f, v->items[i]));
        out = LC_UNIT;
        break;
    }
    case M_keys:
    case M_values:
    case M_items: lc_panic("E0203", site, "`%s` is for maps, got %s", lc_method_names[m], lc_kind(recv, k));
    default: lc_panic("E0203", site, "%s has no method `%s`", lc_kind(recv, k), lc_method_names[m]);
    }
    lc_release(xs);
    return out;
}


lc_v lc_method(int m, lc_v recv, int argc, lc_v *args, const char *site) {
    char k[64];
    lc_buf t = {0}, u = {0};
    if (recv.tag == T_ERR) {
        switch (m) {
        case M_is_err: return LC_TRUE;
        case M_is_ok:
        case M_is_none: return LC_FALSE;
        case M_is_some: return LC_TRUE;
        case M_expect: lc_panic("E0407", site, "%s: %s", argc ? disp(args[0], &u) : "", disp(recv, &t));
        case M_unwrap: lc_panic("E0407", site, "unwrap on an error: %s", disp(recv, &t));
        default: lc_panic("E0406", site, "calling `%s` on an error (%s); add `?`", lc_method_names[m], disp(recv, &t));
        }
    }
    switch (m) {
    case M_is_none: return lc_bool(recv.tag == T_NONE);
    case M_is_some: return lc_bool(recv.tag != T_NONE);
    case M_is_err: return LC_FALSE;
    case M_is_ok: return LC_TRUE;
    case M_unwrap:
    case M_expect:
        if (recv.tag == T_NONE) {
            if (m == M_expect) lc_panic("E0407", site, "%s", argc ? disp(args[0], &t) : "expect");
            lc_panic("E0407", site, "unwrap on none");
        }
        return lc_retain(recv);
    case M_str:
    case M_to_string: return lc_display(recv);
    case M_clone: case M_iter: case M_into_iter: case M_to_owned: case M_copied: case M_cloned: case M_as_str: return lc_retain(recv);
    case M_collect:
        if (recv.tag == T_RANGE || recv.tag == T_SET || recv.tag == T_HEAP) return lc_items(recv, site);
        return lc_retain(recv);
    }
    if (recv.tag == T_NONE) lc_panic("E0407", site, "calling `%s` on none; check `x != none` first or use `x ?? default`", lc_method_names[m]);
    if (recv.tag == T_INT || recv.tag == T_FLOAT) return num_method(m, recv, argc, args, site);
    if (recv.tag == T_STR) {
        lc_v out;
        if (str_method(m, STR(recv), argc, args, site, &out)) return out;
    }
    switch (recv.tag) {
    case T_LIST: {
        lc_vec *v = VEC(recv);
        switch (m) {
        case M_len: return lc_int(v->len);
        case M_is_empty: return lc_bool(v->len == 0);
        case M_first: return v->len ? lc_retain(v->items[0]) : LC_NONE;
        case M_last: return v->len ? lc_retain(v->items[v->len - 1]) : LC_NONE;
        case M_get: {
            int64_t i = int_arg(argc, args, 0, m, site), idx;
            return norm_index(i, v->len, &idx) ? lc_retain(v->items[idx]) : LC_NONE;
        }
        case M_contains:
            for (int64_t i = 0; i < v->len; i++)
                if (lc_eq(v->items[i], args[0]) == 1) return LC_TRUE;
            return LC_FALSE;
        case M_index: {
            lc_v x = arg(argc, args, 0, m, site);
            for (int64_t i = 0; i < v->len; i++)
                if (lc_eq(v->items[i], x) == 1) return lc_int(i);
            return LC_NONE;
        }
        case M_to_list: return lc_retain(recv);
        case M_repeat: return lc_binop(OP_MUL, recv, lc_int(int_arg(argc, args, 0, m, site)), site);
        }
        break;
    }
    case T_MAP: {
        lc_map *mp = MAP(recv);
        switch (m) {
        case M_len: return lc_int(mp->len);
        case M_is_empty: return lc_bool(mp->len == 0);
        case M_keys: {
            lc_v out = lc_list_new(mp->len);
            for (int64_t i = 0; i < mp->len; i++) lc_vec_push(out, lc_retain(mp->e[i].k));
            return out;
        }
        case M_values: {
            lc_v out = lc_list_new(mp->len);
            for (int64_t i = 0; i < mp->len; i++) lc_vec_push(out, lc_retain(mp->e[i].v));
            return out;
        }
        case M_items:
        case M_to_list: return lc_items(recv, site);
        case M_get: {
            lc_entry *e = lc_map_find(mp, arg(argc, args, 0, m, site));
            if (e) return lc_retain(e->v);
            return argc > 1 ? lc_retain(args[1]) : LC_NONE;
        }
        case M_contains:
        case M_contains_key: return lc_bool(lc_map_find(mp, arg(argc, args, 0, m, site)) != NULL);
        case M_to_map: return lc_retain(recv);
        }
        break;
    }
    case T_SET:
        switch (m) {
        case M_len: return lc_int(MAP(recv)->len);
        case M_is_empty: return lc_bool(MAP(recv)->len == 0);
        case M_contains: return lc_bool(lc_contains(recv, arg(argc, args, 0, m, site), site));
        case M_to_set: return lc_retain(recv);
        case M_union:
        case M_intersection:
        case M_difference: {
            lc_v other = lc_collect(0, 1, &args[0], site);
            int op = m == M_union ? OP_BITOR : m == M_intersection ? OP_BITAND : OP_SUB;
            lc_v r = lc_binop(op, recv, other, site);
            lc_release(other);
            return r;
        }
        case M_is_subset: {
            lc_v other = arg(argc, args, 0, m, site);
            lc_map *mp = MAP(recv);
            for (int64_t i = 0; i < mp->len; i++)
                if (!lc_contains(other, mp->e[i].k, site)) return LC_FALSE;
            return LC_TRUE;
        }
        }
        break;
    case T_HEAP:
        switch (m) {
        case M_len: return lc_int(VEC(recv)->len);
        case M_is_empty: return lc_bool(VEC(recv)->len == 0);
        case M_first:
        case M_peek:
        case M_min:
            if (argc == 0) return VEC(recv)->len ? lc_retain(VEC(recv)->items[0]) : LC_NONE;
            break;
        case M_contains: return lc_bool(lc_contains(recv, arg(argc, args, 0, m, site), site));
        }
        break;
    case T_RANGE: {
        lc_range *r = RANGE(recv);
        int64_t n = lc_range_len(r);
        switch (m) {
        case M_len: return lc_int(n);
        case M_is_empty: return lc_bool(n == 0);
        case M_contains: return lc_bool(lc_contains(recv, arg(argc, args, 0, m, site), site));
        case M_rev:
            if (n == 0) return lc_range_new(0, 0, 1);
            return lc_range_new(r->start + r->step * (n - 1), r->start - (r->step > 0 ? 1 : -1), -r->step);
        case M_step_by: {
            int64_t kk = int_arg(argc, args, 0, m, site);
            if (kk <= 0) lc_panic("E0301", site, "step_by needs a positive step");
            return lc_range_new(r->start, r->end, r->step * kk);
        }
        case M_sum:
            if (r->step == 1) {
                __int128 total = (__int128)n * ((__int128)r->start * 2 + n - 1) / 2;
                if (total > INT64_MAX || total < INT64_MIN) lc_panic("E0405", site, "integer overflow in `sum`");
                return lc_int((int64_t)total);
            }
            break;
        }
        break;
    }
    case T_TUPLE:
        if (m == M_len) return lc_int(VEC(recv)->len);
        break;
    case T_STRUCT:
    case T_VARIANT:
    case T_BOOL:
    case T_FUNC:
    case T_UNIT: lc_panic("E0203", site, "%s has no method `%s`", lc_kind(recv, k), lc_method_names[m]);
    }
    return iter_method(m, recv, argc, args, site);
}

/* ----- mutators ----- */

static void need(int argc, int n, int m, const char *site) {
    if (argc < n) lc_panic("E0206", site, "`%s` needs %d argument(s)", lc_method_names[m], n);
}

lc_v lc_mutate(int m, lc_v *slot, int argc, lc_v *args, const char *site) {
    char k[64];
    if (m == M_retain) {
        lc_v kept = lc_method(M_filter, *slot, argc, args, site);
        if (lc_unwinding) return LC_UNIT;
        lc_set(slot, kept);
        return LC_UNIT;
    }
    lc_make_unique(slot);
    lc_v s = *slot;
    switch (s.tag) {
    case T_LIST: {
        lc_vec *v = VEC(s);
        switch (m) {
        case M_push:
            need(argc, 1, m, site);
            for (int i = 0; i < argc; i++) lc_vec_push(s, lc_retain(args[i]));
            return LC_UNIT;
        case M_pop:
            if (argc == 0) {
                if (v->len == 0) return LC_NONE;
                return v->items[--v->len];
            } else {
                int64_t i = lc_int_of(args[0], site), idx;
                if (!norm_index(i, v->len, &idx)) lc_panic("E0402", site, "index %lld out of range for length %lld", (long long)i, (long long)v->len);
                lc_v x = v->items[idx];
                memmove(&v->items[idx], &v->items[idx + 1], sizeof(lc_v) * (v->len - idx - 1));
                v->len--;
                return x;
            }
        case M_insert: {
            need(argc, 2, m, site);
            int64_t i = lc_int_of(args[0], site), len = v->len;
            int64_t idx = i < 0 ? len + i : i;
            if (idx < 0 || idx > len) lc_panic("E0402", site, "insert index %lld out of range for length %lld", (long long)i, (long long)len);
            lc_vec_push(s, LC_UNIT);
            memmove(&v->items[idx + 1], &v->items[idx], sizeof(lc_v) * (len - idx));
            v->items[idx] = lc_retain(args[1]);
            return LC_UNIT;
        }
        case M_remove: {
            need(argc, 1, m, site);
            if (args[0].tag != T_INT)
                lc_panic("E0301", site, "list `remove` takes an index, got %s; to remove a value use `xs.retain(it != x)`", lc_kind(args[0], k));
            int64_t i = args[0].u.i, idx;
            if (!norm_index(i, v->len, &idx)) lc_panic("E0402", site, "index %lld out of range for length %lld", (long long)i, (long long)v->len);
            lc_v x = v->items[idx];
            memmove(&v->items[idx], &v->items[idx + 1], sizeof(lc_v) * (v->len - idx - 1));
            v->len--;
            return x;
        }
        case M_clear:
            for (int64_t i = 0; i < v->len; i++) lc_release(v->items[i]);
            v->len = 0;
            return LC_UNIT;
        case M_extend: {
            need(argc, 1, m, site);
            lc_v ys = lc_items(args[0], site);
            for (int64_t i = 0; i < VEC(ys)->len; i++) lc_vec_push(s, lc_retain(VEC(ys)->items[i]));
            lc_release(ys);
            return LC_UNIT;
        }
        case M_swap: {
            need(argc, 2, m, site);
            int64_t a = lc_int_of(args[0], site), b = lc_int_of(args[1], site), x, y;
            if (!norm_index(a, v->len, &x) || !norm_index(b, v->len, &y))
                lc_panic("E0402", site, "swap index out of range for length %lld", (long long)v->len);
            lc_v t = v->items[x];
            v->items[x] = v->items[y];
            v->items[y] = t;
            return LC_UNIT;
        }
        case M_truncate: {
            need(argc, 1, m, site);
            int64_t n = lc_int_of(args[0], site);
            if (n < 0) n = 0;
            while (v->len > n) lc_release(v->items[--v->len]);
            return LC_UNIT;
        }
        case M_add: lc_panic("E0203", site, "lists use `push`; `add` is for sets");
        default: lc_panic("E0203", site, "list has no method `%s`", lc_method_names[m]);
        }
    }
    case T_MAP:
        switch (m) {
        case M_insert: {
            need(argc, 2, m, site);
            lc_entry *e = lc_map_find(MAP(s), args[0]);
            lc_v old = e ? lc_retain(e->v) : LC_NONE;
            lc_map_put(s, lc_retain(args[0]), lc_retain(args[1]));
            return old;
        }
        case M_remove:
        case M_pop: {
            need(argc, 1, m, site);
            bool found;
            lc_v v = lc_map_remove(s, args[0], &found);
            return found ? v : LC_NONE;
        }
        case M_clear: lc_map_clear(s); return LC_UNIT;
        case M_extend: {
            need(argc, 1, m, site);
            if (args[0].tag == T_MAP) {
                lc_map *o = MAP(args[0]);
                for (int64_t i = 0; i < o->len; i++) lc_map_put(s, lc_retain(o->e[i].k), lc_retain(o->e[i].v));
            } else {
                lc_v ys = lc_items(args[0], site);
                for (int64_t i = 0; i < VEC(ys)->len; i++) {
                    lc_v x = VEC(ys)->items[i];
                    if (x.tag != T_TUPLE || VEC(x)->len != 2) lc_panic("E0301", site, "map `extend` needs a map or (key, value) pairs");
                    lc_map_put(s, lc_retain(VEC(x)->items[0]), lc_retain(VEC(x)->items[1]));
                }
                lc_release(ys);
            }
            return LC_UNIT;
        }
        case M_push:
        case M_add: lc_panic("E0203", site, "maps have no `%s`; set entries with `m[k] = v`", lc_method_names[m]);
        default: lc_panic("E0203", site, "map has no method `%s`", lc_method_names[m]);
        }
    case T_HEAP:
        switch (m) {
        case M_push:
        case M_add:
            need(argc, 1, m, site);
            for (int i = 0; i < argc; i++) lc_heap_push(s, lc_retain(args[i]), site);
            return LC_UNIT;
        case M_pop: return heap_pop(s);
        case M_clear:
            for (int64_t i = 0; i < VEC(s)->len; i++) lc_release(VEC(s)->items[i]);
            VEC(s)->len = 0;
            return LC_UNIT;
        case M_extend: {
            need(argc, 1, m, site);
            lc_v ys = lc_items(args[0], site);
            for (int64_t i = 0; i < VEC(ys)->len; i++) lc_heap_push(s, lc_retain(VEC(ys)->items[i]), site);
            lc_release(ys);
            return LC_UNIT;
        }
        default: lc_panic("E0203", site, "heap has no method `%s`; it has push, pop, first, len, extend, clear", lc_method_names[m]);
        }
    case T_SET:
        switch (m) {
        case M_add:
        case M_insert:
            need(argc, 1, m, site);
            return lc_bool(lc_set_add(s, lc_retain(args[argc - 1])));
        case M_remove: {
            need(argc, 1, m, site);
            bool found;
            lc_v v = lc_map_remove(s, args[0], &found);
            if (found) lc_release(v);
            return lc_bool(found);
        }
        case M_clear: lc_map_clear(s); return LC_UNIT;
        case M_extend: {
            need(argc, 1, m, site);
            lc_v ys = lc_items(args[0], site);
            for (int64_t i = 0; i < VEC(ys)->len; i++) lc_set_add(s, lc_retain(VEC(ys)->items[i]));
            lc_release(ys);
            return LC_UNIT;
        }
        case M_push: lc_panic("E0203", site, "sets use `add`");
        default: lc_panic("E0203", site, "set has no method `%s`", lc_method_names[m]);
        }
    case T_STR: lc_panic("E0410", site, "strings are immutable, so there is no `%s`; build a new one: `s += \"x\"`", lc_method_names[m]);
    case T_NONE: lc_panic("E0407", site, "calling `%s` on none", lc_method_names[m]);
    }
    lc_panic("E0203", site, "%s has no method `%s`", lc_kind(s, k), lc_method_names[m]);
}
