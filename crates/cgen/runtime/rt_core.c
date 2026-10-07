/* Core runtime: memory, strings, collections, equality, ordering, hashing,
 * display, panics, the call stack and output. Mirrors crates/interp/src/value.rs. */
#include "lacon.h"

#include <errno.h>
#include <math.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>

const lc_program *lc_prog;
const char *lc_callsite;
int64_t lc_depth;
int lc_unwinding;
lc_v lc_unwind_val;

static void *xmalloc(size_t n) {
    void *p = malloc(n ? n : 1);
    if (!p) {
        fputs("out of memory\n", stderr);
        exit(1);
    }
    return p;
}

static void *xrealloc(void *p, size_t n) {
    p = realloc(p, n ? n : 1);
    if (!p) {
        fputs("out of memory\n", stderr);
        exit(1);
    }
    return p;
}

/* ----- memory -----
 *
 * Values and their storage come from here. A block of up to SMALL_MAX
 * bytes is popped from the free list of its size class (a multiple of 16
 * bytes), or carved from a chunk when the list is empty, and is pushed back
 * when freed: no malloc, no lock and no header. Freeing takes the size,
 * which every object knows (a string's cap, a list's kind and cap). Freed
 * blocks wait on their list for the next block of that class and are never
 * given back to malloc. The runtime runs on one thread, so the lists are
 * plain globals. Larger blocks are malloc's.
 *
 * Under AddressSanitizer every block comes from malloc instead, so the
 * sanitizer still sees each one, with its size in a header that freeing
 * checks. */

#define SMALL_MAX 512
#define CHUNK_MIN ((size_t)64 << 10)
#define CHUNK_MAX ((size_t)4 << 20)

#ifdef __SANITIZE_ADDRESS__

static void *mem_alloc(size_t n) {
    size_t *p = xmalloc(n + 16);
    *p = n;
    return (char *)p + 16;
}

static void mem_free(void *p, size_t n) {
    if (!p) return;
    size_t *h = (size_t *)((char *)p - 16);
    if (*h != n) {
        fprintf(stderr, "lacon runtime: freeing %zu bytes of a %zu-byte block\n", n, *h);
        abort();
    }
    free(h);
}

#else

typedef struct free_block { struct free_block *next; } free_block;
static free_block *free_lists[SMALL_MAX / 16 + 1];
static char *chunk_at, *chunk_end;
static size_t chunk_size = CHUNK_MIN;

static inline __attribute__((always_inline)) size_t size_class(size_t n) { return n ? (n + 15) >> 4 : 1; }

/* A block of `c` * 16 bytes from a new chunk. What is left of the old
 * chunk, a whole number of classes, goes on the free list of its size. */
static __attribute__((noinline)) void *chunk_carve(size_t c) {
    size_t left = (size_t)(chunk_end - chunk_at);
    if (left) {
        free_block *b = (free_block *)chunk_at;
        b->next = free_lists[left >> 4];
        free_lists[left >> 4] = b;
    }
    chunk_at = xmalloc(chunk_size);
    chunk_end = chunk_at + chunk_size;
    if (chunk_size < CHUNK_MAX) chunk_size *= 2;
    void *p = chunk_at;
    chunk_at += c << 4;
    return p;
}

static inline __attribute__((always_inline)) void *mem_alloc(size_t n) {
    if (__builtin_expect(n > SMALL_MAX, 0)) return xmalloc(n);
    size_t c = size_class(n);
    free_block *b = free_lists[c];
    if (__builtin_expect(b != NULL, 1)) {
        free_lists[c] = b->next;
        return b;
    }
    if (__builtin_expect((size_t)(chunk_end - chunk_at) >= c << 4, 1)) {
        void *p = chunk_at;
        chunk_at += c << 4;
        return p;
    }
    return chunk_carve(c);
}

/* Frees a block of `n` bytes from `mem_alloc`, or nothing for NULL. */
static inline __attribute__((always_inline)) void mem_free(void *p, size_t n) {
    if (__builtin_expect(n > SMALL_MAX, 0)) {
        free(p);
        return;
    }
    if (!p) return;
    free_block *b = p;
    size_t c = size_class(n);
    b->next = free_lists[c];
    free_lists[c] = b;
}

#endif

/* A block of `old` bytes (or NULL) grown or shrunk to `n` bytes. */
static void *mem_realloc(void *p, size_t old, size_t n) {
    if (!p) return mem_alloc(n);
#ifndef __SANITIZE_ADDRESS__
    if (old > SMALL_MAX && n > SMALL_MAX) return xrealloc(p, n);
    if (old <= SMALL_MAX && n <= SMALL_MAX && size_class(old) == size_class(n)) return p;
#endif
    void *q = mem_alloc(n);
    memcpy(q, p, old < n ? old : n);
    mem_free(p, old);
    return q;
}

#define STR_SIZE(cap) (sizeof(lc_str) + (size_t)(cap) + 1)
#define VEC_DATA_SIZE(kind, cap) (lc_ksize(kind) * (size_t)(cap))

/* ----- freeing ----- */

void lc_free(lc_v v) {
    switch (v.tag) {
    case T_STR:
        mem_free(v.u.o, STR_SIZE(STR(v)->cap));
        break;
    case T_RANGE:
        mem_free(v.u.o, sizeof(lc_range));
        break;
    case T_LIST:
    case T_TUPLE:
    case T_HEAP: {
        lc_vec *x = VEC(v);
        if (x->kind == K_BOXED)
            for (int64_t i = 0; i < x->len; i++) lc_release(x->boxed[i]);
        mem_free(x->data, VEC_DATA_SIZE(x->kind, x->cap));
        mem_free(x, sizeof(lc_vec));
        break;
    }
    case T_MAP:
    case T_SET: {
        lc_map *m = MAP(v);
        for (int64_t i = 0; i < m->len; i++) {
            lc_release(m->e[i].k);
            lc_release(m->e[i].v);
        }
        mem_free(m->e, sizeof(lc_entry) * m->cap);
        mem_free(m->idx, sizeof(int32_t) * m->icap);
        mem_free(m, sizeof(lc_map));
        break;
    }
    case T_STRUCT:
    case T_VARIANT: {
        lc_rec *r = REC(v);
        for (int64_t i = 0; i < r->n; i++) lc_release(r->f[i]);
        mem_free(r, sizeof(lc_rec) + sizeof(lc_v) * r->n);
        break;
    }
    case T_ERR:
        lc_release(ERRV(v)->payload);
        mem_free(v.u.o, sizeof(lc_err));
        break;
    case T_FUNC: {
        lc_fn *f = FN(v);
        for (int i = 0; i < f->nenv; i++) lc_release(f->env[i]);
        mem_free(f, sizeof(lc_fn) + sizeof(lc_v) * f->nenv);
        break;
    }
    case T_FRAME: {
        lc_frame *f = FRAME(v);
        for (int64_t i = 0; i < f->n; i++) lc_release(f->slots[i]);
        if (f->parent) lc_release(lc_obj_v(T_FRAME, f->parent));
        mem_free(f, sizeof(lc_frame) + sizeof(lc_v) * f->n);
        break;
    }
    }
}

lc_frame *lc_frame_new(int64_t n, lc_frame *parent) {
    lc_frame *f = mem_alloc(sizeof(lc_frame) + sizeof(lc_v) * n);
    f->rc = 1;
    f->parent = parent;
    if (parent) parent->rc++;
    f->n = n;
    for (int64_t i = 0; i < n; i++) f->slots[i] = LC_UNIT;
    return f;
}

/* ----- strings and buffers ----- */

static int64_t count_chars(const char *s, int64_t len) {
    int64_t n = 0;
    for (int64_t i = 0; i < len; i++)
        if (((unsigned char)s[i] & 0xC0) != 0x80) n++;
    return n;
}

lc_v lc_str_new(const char *s, int64_t len) {
    lc_str *x = mem_alloc(STR_SIZE(len));
    x->rc = 1;
    x->len = len;
    x->cap = len;
    if (len) memcpy(x->data, s, len);
    x->data[len] = 0;
    x->nchars = count_chars(x->data, len);
    return lc_obj_v(T_STR, x);
}

lc_v lc_cstr(const char *s) { return lc_str_new(s, (int64_t)strlen(s)); }

/* `a` then `b` as a new string, in one allocation. */
lc_v lc_str_cat(lc_str *a, lc_str *b) {
    int64_t len = a->len + b->len;
    lc_str *x = mem_alloc(STR_SIZE(len));
    x->rc = 1;
    x->len = len;
    x->cap = len;
    memcpy(x->data, a->data, a->len);
    memcpy(x->data + a->len, b->data, b->len);
    x->data[len] = 0;
    x->nchars = a->nchars + b->nchars;
    return lc_obj_v(T_STR, x);
}

/* The decimal digits of `v` into `buf` (at least 21 bytes), returning the
 * length; what `%lld` gives, without printf. */
int lc_fmt_int(char *buf, int64_t v) {
    char t[24];
    int n = 0;
    uint64_t u = v < 0 ? (uint64_t)0 - (uint64_t)v : (uint64_t)v;
    do {
        t[n++] = (char)('0' + u % 10);
        u /= 10;
    } while (u);
    int len = 0;
    if (v < 0) buf[len++] = '-';
    while (n) buf[len++] = t[--n];
    buf[len] = 0;
    return len;
}

lc_v lc_str_lit(const char *s, int64_t len) {
    lc_v v = lc_str_new(s, len);
    v.u.o->rc = LC_IMMORTAL;
    return v;
}

int64_t lc_str_nchars(lc_str *s) { return s->nchars; }

/* Appends to a string nothing else holds; may move it. */
lc_v lc_str_append(lc_v a, const char *s, int64_t n) {
    lc_str *x = STR(a);
    if (x->len + n > x->cap) {
        int64_t cap = x->cap * 2 > x->len + n ? x->cap * 2 : x->len + n;
        x = mem_realloc(x, STR_SIZE(x->cap), STR_SIZE(cap));
        x->cap = cap;
    }
    memcpy(x->data + x->len, s, n);
    x->len += n;
    x->data[x->len] = 0;
    x->nchars += count_chars(s, n);
    return lc_obj_v(T_STR, x);
}

void lc_buf_put(lc_buf *b, const char *s, int64_t n) {
    if (b->len + n + 1 > b->cap) {
        int64_t cap = b->cap ? b->cap * 2 : 64;
        while (cap < b->len + n + 1) cap *= 2;
        b->p = xrealloc(b->p, cap);
        b->cap = cap;
    }
    memcpy(b->p + b->len, s, n);
    b->len += n;
    b->p[b->len] = 0;
}

void lc_buf_puts(lc_buf *b, const char *s) { lc_buf_put(b, s, (int64_t)strlen(s)); }
void lc_buf_putc(lc_buf *b, char c) { lc_buf_put(b, &c, 1); }

lc_v lc_buf_finish(lc_buf *b) {
    lc_v v = lc_str_new(b->p ? b->p : "", b->len);
    free(b->p);
    b->p = NULL;
    b->len = b->cap = 0;
    return v;
}

/* ----- collections ----- */

static int kind_of(lc_v v) {
    switch (v.tag) {
    case T_INT: return K_INT;
    case T_FLOAT: return K_FLOAT;
    case T_BOOL: return K_BOOL;
    }
    return K_BOXED;
}

lc_v lc_vec_alloc(uint32_t tag, int kind, int64_t cap) {
    lc_vec *x = mem_alloc(sizeof(lc_vec));
    x->rc = 1;
    x->len = 0;
    x->cap = cap;
    x->kind = kind;
    x->data = cap ? mem_alloc(VEC_DATA_SIZE(kind, cap)) : NULL;
    return lc_obj_v(tag, x);
}

lc_v lc_list_new(int64_t cap) { return lc_vec_alloc(T_LIST, K_BOXED, cap); }
lc_v lc_tuple_new(int64_t cap) { return lc_vec_alloc(T_TUPLE, K_BOXED, cap); }
lc_v lc_heap_new(void) { return lc_vec_alloc(T_HEAP, K_BOXED, 0); }

void lc_vec_box(lc_vec *x) {
    if (x->kind == K_BOXED) return;
    lc_v *b = x->cap ? mem_alloc(VEC_DATA_SIZE(K_BOXED, x->cap)) : NULL;
    for (int64_t i = 0; i < x->len; i++) b[i] = lc_vget(x, i);
    mem_free(x->data, VEC_DATA_SIZE(x->kind, x->cap));
    x->boxed = b;
    x->kind = K_BOXED;
}

void lc_vec_push(lc_v vec, lc_v item) {
    lc_vec *x = VEC(vec);
    int k = kind_of(item);
    if (k != x->kind) {
        if (x->len == 0 && vec.tag == T_LIST) {
            if (x->cap) x->data = mem_realloc(x->data, VEC_DATA_SIZE(x->kind, x->cap), VEC_DATA_SIZE(k, x->cap));
            x->kind = k;
        } else {
            lc_vec_box(x);
        }
    }
    if (x->len == x->cap) {
        int64_t cap = x->cap ? x->cap * 2 : 4;
        x->data = mem_realloc(x->data, VEC_DATA_SIZE(x->kind, x->cap), VEC_DATA_SIZE(x->kind, cap));
        x->cap = cap;
    }
    switch (x->kind) {
    case K_INT: x->ints[x->len++] = item.u.i; break;
    case K_FLOAT: x->floats[x->len++] = item.u.f; break;
    case K_BOOL: x->bools[x->len++] = item.u.i != 0; break;
    default: x->boxed[x->len++] = item;
    }
}

void lc_vset(lc_vec *x, int64_t i, lc_v v) {
    if (x->kind != K_BOXED && kind_of(v) != x->kind) lc_vec_box(x);
    switch (x->kind) {
    case K_INT: x->ints[i] = v.u.i; break;
    case K_FLOAT: x->floats[i] = v.u.f; break;
    case K_BOOL: x->bools[i] = v.u.i != 0; break;
    default: lc_set(&x->boxed[i], v);
    }
}

lc_v lc_vec_slice(lc_v src, uint32_t tag, int64_t a, int64_t b) {
    lc_vec *x = VEC(src);
    if (x->kind != K_BOXED && tag == T_LIST) {
        lc_v out = lc_vec_alloc(T_LIST, x->kind, b - a);
        if (b > a) memcpy(VEC(out)->data, (char *)x->data + lc_ksize(x->kind) * a, lc_ksize(x->kind) * (b - a));
        VEC(out)->len = b - a;
        return out;
    }
    lc_v out = lc_vec_alloc(tag, K_BOXED, b - a);
    for (int64_t i = a; i < b; i++) lc_vec_push(out, lc_retain(lc_vget(x, i)));
    return out;
}

lc_v lc_list_of(int n, lc_v *items) {
    lc_v v = lc_list_new(n);
    for (int i = 0; i < n; i++) lc_vec_push(v, items[i]);
    return v;
}

lc_v lc_tuple_of(int n, lc_v *items) {
    lc_v v = lc_tuple_new(n);
    for (int i = 0; i < n; i++) lc_vec_push(v, items[i]);
    return v;
}

static lc_v map_alloc(uint32_t tag) {
    lc_map *m = mem_alloc(sizeof(lc_map));
    m->rc = 1;
    m->len = m->cap = 0;
    m->e = NULL;
    m->idx = NULL;
    m->icap = 0;
    return lc_obj_v(tag, m);
}

lc_v lc_map_new(void) { return map_alloc(T_MAP); }
lc_v lc_set_new(void) { return map_alloc(T_SET); }

static void map_reindex(lc_map *m) {
    int64_t icap = 8;
    while (icap < m->cap * 2) icap *= 2;
    mem_free(m->idx, sizeof(int32_t) * m->icap);
    m->idx = mem_alloc(sizeof(int32_t) * icap);
    m->icap = icap;
    for (int64_t i = 0; i < icap; i++) m->idx[i] = -1;
    for (int64_t i = 0; i < m->len; i++) {
        uint64_t j = m->e[i].h & (icap - 1);
        while (m->idx[j] >= 0) j = (j + 1) & (icap - 1);
        m->idx[j] = (int32_t)i;
    }
}

/* The slot in the index for `k`: either holding its entry, or empty. */
static int64_t map_slot(lc_map *m, lc_v k, uint64_t h) {
    uint64_t j = h & (m->icap - 1);
    for (;;) {
        int32_t i = m->idx[j];
        if (i < 0) return (int64_t)j;
        if (m->e[i].h == h && lc_key_eq(m->e[i].k, k)) return (int64_t)j;
        j = (j + 1) & (m->icap - 1);
    }
}

lc_entry *lc_map_find(lc_map *m, lc_v k) {
    if (m->len == 0) return NULL;
    uint64_t h = lc_hash(k);
    int32_t i = m->idx[map_slot(m, k, h)];
    return i < 0 ? NULL : &m->e[i];
}

/* Inserts an owned key and value; an existing key keeps its place and the
 * new key is released. Returns the entry. */
static lc_entry *map_insert(lc_map *m, lc_v k, lc_v v, bool *existed) {
    uint64_t h = lc_hash(k);
    if (m->icap) {
        int32_t i = m->idx[map_slot(m, k, h)];
        if (i >= 0) {
            lc_release(k);
            lc_set(&m->e[i].v, v);
            *existed = true;
            return &m->e[i];
        }
    }
    *existed = false;
    if (m->len == m->cap) {
        int64_t cap = m->cap ? m->cap * 2 : 4;
        m->e = mem_realloc(m->e, sizeof(lc_entry) * m->cap, sizeof(lc_entry) * cap);
        m->cap = cap;
    }
    m->e[m->len] = (lc_entry){k, v, h};
    m->len++;
    if (m->icap < m->cap * 2) {
        map_reindex(m);
    } else {
        int64_t j = map_slot(m, k, h);
        m->idx[j] = (int32_t)(m->len - 1);
    }
    return &m->e[m->len - 1];
}

void lc_map_put(lc_v map, lc_v k, lc_v v) {
    bool existed;
    map_insert(MAP(map), k, v, &existed);
}

bool lc_set_add(lc_v set, lc_v k) {
    bool existed;
    map_insert(MAP(set), k, LC_UNIT, &existed);
    return !existed;
}

lc_entry *lc_map_insert_entry(lc_v map, lc_v k, lc_v v, bool *existed) { return map_insert(MAP(map), k, v, existed); }

/* Removes `k`, keeping the order of the rest. Returns the value (owned) or
 * sets *found to false. */
lc_v lc_map_remove(lc_v map, lc_v k, bool *found) {
    lc_map *m = MAP(map);
    lc_entry *e = lc_map_find(m, k);
    if (!e) {
        *found = false;
        return LC_NONE;
    }
    *found = true;
    int64_t i = e - m->e;
    lc_v v = e->v;
    lc_release(e->k);
    memmove(&m->e[i], &m->e[i + 1], sizeof(lc_entry) * (m->len - i - 1));
    m->len--;
    map_reindex(m);
    return v;
}

void lc_map_clear(lc_v map) {
    lc_map *m = MAP(map);
    for (int64_t i = 0; i < m->len; i++) {
        lc_release(m->e[i].k);
        lc_release(m->e[i].v);
    }
    m->len = 0;
    if (m->icap) map_reindex(m);
}

lc_v lc_rec_new(uint32_t tag, uint32_t ty, uint32_t vtag, int64_t n, lc_v *fields) {
    lc_rec *r = mem_alloc(sizeof(lc_rec) + sizeof(lc_v) * n);
    r->rc = 1;
    r->ty = ty;
    r->tag = vtag;
    r->n = n;
    for (int64_t i = 0; i < n; i++) r->f[i] = fields[i];
    return lc_obj_v(tag, r);
}

lc_v lc_range_new(int64_t start, int64_t end, int64_t step) {
    lc_range *r = mem_alloc(sizeof(lc_range));
    r->rc = 1;
    r->start = start;
    r->end = end;
    r->step = step;
    return lc_obj_v(T_RANGE, r);
}

int64_t lc_range_len(lc_range *r) {
    if (r->step > 0) return r->end <= r->start ? 0 : (r->end - r->start - 1) / r->step + 1;
    return r->end >= r->start ? 0 : (r->start - r->end - 1) / (-r->step) + 1;
}

lc_v lc_err_new(lc_v payload) {
    lc_err *e = mem_alloc(sizeof(lc_err));
    e->rc = 1;
    e->payload = payload;
    return lc_obj_v(T_ERR, e);
}

lc_v lc_closure_new(lc_code code, int nparams, int nenv, lc_v *env) {
    lc_fn *f = mem_alloc(sizeof(lc_fn) + sizeof(lc_v) * nenv);
    f->rc = 1;
    f->kind = FN_CLOSURE;
    f->id = f->tag = 0;
    f->nparams = nparams;
    f->code = code;
    f->nenv = nenv;
    for (int i = 0; i < nenv; i++) f->env[i] = env[i];
    return lc_obj_v(T_FUNC, f);
}

lc_v lc_fnval_new(int kind, int id, int tag, lc_code code) {
    lc_fn *f = mem_alloc(sizeof(lc_fn));
    f->rc = 1;
    f->kind = kind;
    f->id = id;
    f->tag = tag;
    f->nparams = 0;
    f->code = code;
    f->nenv = 0;
    return lc_obj_v(T_FUNC, f);
}

/* ----- equality, ordering, hashing ----- */

static lc_v range_list(lc_range *r) {
    int64_t n = lc_range_len(r);
    lc_v l = lc_list_new(n);
    for (int64_t i = 0; i < n; i++) lc_vec_push(l, lc_int(r->start + r->step * i));
    return l;
}

lc_v lc_heap_sorted(lc_v h);

int lc_eq(lc_v a, lc_v b) {
    if (a.tag == T_NONE || b.tag == T_NONE) return a.tag == b.tag;
    if (a.tag == T_UNIT && b.tag == T_UNIT) return 1;
    if (a.tag == T_ERR && b.tag == T_ERR) return lc_eq(ERRV(a)->payload, ERRV(b)->payload);
    if (a.tag == T_ERR || b.tag == T_ERR) return 0;
    switch (a.tag) {
    case T_BOOL:
        if (b.tag == T_BOOL) return a.u.i == b.u.i;
        break;
    case T_INT:
        if (b.tag == T_INT) return a.u.i == b.u.i;
        if (b.tag == T_FLOAT) return (double)a.u.i == b.u.f;
        break;
    case T_FLOAT:
        if (b.tag == T_FLOAT) return a.u.f == b.u.f;
        if (b.tag == T_INT) return a.u.f == (double)b.u.i;
        break;
    case T_STR:
        if (b.tag == T_STR) return STR(a)->len == STR(b)->len && memcmp(STR(a)->data, STR(b)->data, STR(a)->len) == 0;
        break;
    case T_LIST:
    case T_TUPLE:
        if (b.tag == a.tag) {
            if (a.u.o == b.u.o) return 1;
            lc_vec *x = VEC(a), *y = VEC(b);
            if (x->len != y->len) return 0;
            for (int64_t i = 0; i < x->len; i++) {
                int r = lc_eq(lc_vget(x, i), lc_vget(y, i));
                if (r != 1) return r;
            }
            return 1;
        }
        if (a.tag == T_LIST && b.tag == T_RANGE) {
            lc_v l = range_list(RANGE(b));
            int r = lc_eq(a, l);
            lc_release(l);
            return r;
        }
        break;
    case T_MAP:
        if (b.tag == T_MAP) {
            lc_map *x = MAP(a), *y = MAP(b);
            if (x->len != y->len) return 0;
            for (int64_t i = 0; i < x->len; i++) {
                lc_entry *e = lc_map_find(y, x->e[i].k);
                if (!e) return 0;
                int r = lc_eq(x->e[i].v, e->v);
                if (r != 1) return r;
            }
            return 1;
        }
        break;
    case T_SET:
        if (b.tag == T_SET) {
            lc_map *x = MAP(a), *y = MAP(b);
            if (x->len != y->len) return 0;
            for (int64_t i = 0; i < x->len; i++)
                if (!lc_map_find(y, x->e[i].k)) return 0;
            return 1;
        }
        break;
    case T_HEAP:
        if (b.tag == T_HEAP) {
            lc_v x = lc_heap_sorted(a), y = lc_heap_sorted(b);
            int r = lc_eq(x, y);
            lc_release(x);
            lc_release(y);
            return r;
        }
        break;
    case T_STRUCT:
    case T_VARIANT:
        if (b.tag == a.tag) {
            lc_rec *x = REC(a), *y = REC(b);
            if (x->ty != y->ty || x->tag != y->tag) return 0;
            for (int64_t i = 0; i < x->n && i < y->n; i++) {
                int r = lc_eq(x->f[i], y->f[i]);
                if (r != 1) return r;
            }
            return 1;
        }
        break;
    case T_RANGE:
        if (b.tag == T_RANGE) {
            lc_range *x = RANGE(a), *y = RANGE(b);
            return x->start == y->start && x->end == y->end && x->step == y->step;
        }
        if (b.tag == T_LIST) return lc_eq(b, a);
        break;
    case T_FUNC:
        if (b.tag == T_FUNC) return a.u.o == b.u.o;
        break;
    }
    return -1;
}

static int sgn(int64_t x) { return (x > 0) - (x < 0); }

int lc_cmp(lc_v a, lc_v b, bool *ok) {
    *ok = true;
    switch (a.tag) {
    case T_INT:
        if (b.tag == T_INT) return sgn((a.u.i > b.u.i) - (a.u.i < b.u.i));
        if (b.tag == T_FLOAT) {
            double x = (double)a.u.i;
            if (isnan(b.u.f)) break;
            return (x > b.u.f) - (x < b.u.f);
        }
        break;
    case T_FLOAT:
        if (b.tag == T_FLOAT || b.tag == T_INT) {
            double y = b.tag == T_INT ? (double)b.u.i : b.u.f;
            if (isnan(a.u.f) || isnan(y)) break;
            return (a.u.f > y) - (a.u.f < y);
        }
        break;
    case T_STR:
        if (b.tag == T_STR) {
            lc_str *x = STR(a), *y = STR(b);
            int64_t n = x->len < y->len ? x->len : y->len;
            int r = memcmp(x->data, y->data, n);
            if (r) return r < 0 ? -1 : 1;
            return sgn(x->len - y->len);
        }
        break;
    case T_BOOL:
        if (b.tag == T_BOOL) return sgn(a.u.i - b.u.i);
        break;
    case T_UNIT:
        if (b.tag == T_UNIT) return 0;
        break;
    case T_LIST:
    case T_TUPLE:
        if (b.tag == a.tag) {
            lc_vec *x = VEC(a), *y = VEC(b);
            for (int64_t i = 0; i < x->len && i < y->len; i++) {
                int r = lc_cmp(lc_vget(x, i), lc_vget(y, i), ok);
                if (!*ok) return 0;
                if (r) return r;
            }
            return sgn(x->len - y->len);
        }
        break;
    case T_STRUCT:
    case T_VARIANT:
        if (b.tag == a.tag && REC(a)->ty == REC(b)->ty) {
            lc_rec *x = REC(a), *y = REC(b);
            if (x->tag != y->tag) return x->tag < y->tag ? -1 : 1;
            for (int64_t i = 0; i < x->n && i < y->n; i++) {
                int r = lc_cmp(x->f[i], y->f[i], ok);
                if (!*ok) return 0;
                if (r) return r;
            }
            return 0;
        }
        break;
    case T_NONE:
        return b.tag == T_NONE ? 0 : -1;
    }
    if (b.tag == T_NONE) return 1;
    *ok = false;
    return 0;
}

static double norm(double f) { return f == 0.0 ? 0.0 : f; }

bool lc_key_eq(lc_v a, lc_v b) {
    if (a.tag != b.tag) return false;
    switch (a.tag) {
    case T_UNIT:
    case T_NONE:
        return true;
    case T_BOOL:
    case T_INT:
        return a.u.i == b.u.i;
    case T_FLOAT: {
        double x = norm(a.u.f), y = norm(b.u.f);
        return memcmp(&x, &y, sizeof x) == 0;
    }
    case T_STR:
        return STR(a)->len == STR(b)->len && memcmp(STR(a)->data, STR(b)->data, STR(a)->len) == 0;
    case T_LIST:
    case T_TUPLE: {
        lc_vec *x = VEC(a), *y = VEC(b);
        if (x->len != y->len) return false;
        for (int64_t i = 0; i < x->len; i++)
            if (!lc_key_eq(lc_vget(x, i), lc_vget(y, i))) return false;
        return true;
    }
    case T_MAP: {
        lc_map *x = MAP(a), *y = MAP(b);
        if (x->len != y->len) return false;
        for (int64_t i = 0; i < x->len; i++) {
            lc_entry *e = lc_map_find(y, x->e[i].k);
            if (!e || !lc_key_eq(x->e[i].v, e->v)) return false;
        }
        return true;
    }
    case T_SET: {
        lc_map *x = MAP(a), *y = MAP(b);
        if (x->len != y->len) return false;
        for (int64_t i = 0; i < x->len; i++)
            if (!lc_map_find(y, x->e[i].k)) return false;
        return true;
    }
    case T_HEAP: {
        lc_v x = lc_heap_sorted(a), y = lc_heap_sorted(b);
        bool r = lc_key_eq(x, y);
        lc_release(x);
        lc_release(y);
        return r;
    }
    case T_STRUCT:
    case T_VARIANT: {
        lc_rec *x = REC(a), *y = REC(b);
        if (x->ty != y->ty || x->tag != y->tag || x->n != y->n) return false;
        for (int64_t i = 0; i < x->n; i++)
            if (!lc_key_eq(x->f[i], y->f[i])) return false;
        return true;
    }
    case T_RANGE: {
        lc_range *x = RANGE(a), *y = RANGE(b);
        return x->start == y->start && x->end == y->end && x->step == y->step;
    }
    case T_ERR:
        return lc_key_eq(ERRV(a)->payload, ERRV(b)->payload);
    case T_FUNC:
        return a.u.o == b.u.o;
    }
    return false;
}

static uint64_t mix(uint64_t h, uint64_t x) {
    h ^= x + 0x9e3779b97f4a7c15ULL + (h << 6) + (h >> 2);
    h *= 0xff51afd7ed558ccdULL;
    return h ^ (h >> 33);
}

uint64_t lc_hash(lc_v v) {
    uint64_t h = mix(0x84222325cbf29ce4ULL, v.tag);
    switch (v.tag) {
    case T_BOOL:
    case T_INT:
        return mix(h, (uint64_t)v.u.i);
    case T_FLOAT: {
        double f = norm(v.u.f);
        uint64_t bits;
        memcpy(&bits, &f, sizeof bits);
        return mix(h, bits);
    }
    case T_STR: {
        lc_str *s = STR(v);
        uint64_t x = 0xcbf29ce484222325ULL;
        for (int64_t i = 0; i < s->len; i++) x = (x ^ (unsigned char)s->data[i]) * 0x100000001b3ULL;
        return mix(h, x);
    }
    case T_LIST:
    case T_TUPLE: {
        lc_vec *x = VEC(v);
        h = mix(h, (uint64_t)x->len);
        for (int64_t i = 0; i < x->len; i++) h = mix(h, lc_hash(lc_vget(x, i)));
        return h;
    }
    case T_MAP:
    case T_SET:
        return mix(h, (uint64_t)MAP(v)->len);
    case T_HEAP:
        return mix(h, (uint64_t)VEC(v)->len);
    case T_STRUCT:
    case T_VARIANT: {
        lc_rec *r = REC(v);
        h = mix(mix(h, r->ty), r->tag);
        for (int64_t i = 0; i < r->n; i++) h = mix(h, lc_hash(r->f[i]));
        return h;
    }
    case T_RANGE:
        return mix(mix(mix(h, RANGE(v)->start), RANGE(v)->end), RANGE(v)->step);
    case T_ERR:
        return mix(h, lc_hash(ERRV(v)->payload));
    case T_FUNC:
        return mix(h, (uint64_t)(uintptr_t)v.u.o);
    }
    return h;
}

/* A stable merge sort; sets *bad on incomparable items. */
static char g_bad_a[64], g_bad_b[64];
static bool g_sort_ok;

static void merge_sort(lc_kv *xs, lc_kv *tmp, int64_t n) {
    if (n < 2) return;
    int64_t m = n / 2;
    merge_sort(xs, tmp, m);
    merge_sort(xs + m, tmp, n - m);
    int64_t i = 0, j = m, k = 0;
    while (i < m && j < n) {
        bool ok;
        int c = lc_cmp(xs[j].key, xs[i].key, &ok);
        if (!ok && g_sort_ok) {
            g_sort_ok = false;
            char ka[64], kb[64];
            snprintf(g_bad_a, sizeof g_bad_a, "%s", lc_kind(xs[j].key, ka));
            snprintf(g_bad_b, sizeof g_bad_b, "%s", lc_kind(xs[i].key, kb));
        }
        if (ok && c < 0)
            tmp[k++] = xs[j++];
        else
            tmp[k++] = xs[i++];
    }
    while (i < m) tmp[k++] = xs[i++];
    while (j < n) tmp[k++] = xs[j++];
    memcpy(xs, tmp, sizeof(lc_kv) * n);
}

/* `merge_sort` for int keys, which always compare: the same stable order. */
static void merge_sort_ints(lc_kv *xs, lc_kv *tmp, int64_t n) {
    if (n <= 16) {
        for (int64_t i = 1; i < n; i++) {
            lc_kv x = xs[i];
            int64_t j = i;
            while (j > 0 && x.key.u.i < xs[j - 1].key.u.i) {
                xs[j] = xs[j - 1];
                j--;
            }
            xs[j] = x;
        }
        return;
    }
    int64_t m = n / 2;
    merge_sort_ints(xs, tmp, m);
    merge_sort_ints(xs + m, tmp, n - m);
    int64_t i = 0, j = m, k = 0;
    while (i < m && j < n) tmp[k++] = xs[j].key.u.i < xs[i].key.u.i ? xs[j++] : xs[i++];
    while (i < m) tmp[k++] = xs[i++];
    while (j < n) tmp[k++] = xs[j++];
    memcpy(xs, tmp, sizeof(lc_kv) * n);
}

/* A stable LSD radix sort of pairs with int keys, by each key's offset from
 * the smallest, 11 bits a pass. Returns false, sorting nothing, when
 * `merge_sort_ints` would be faster: below 4096 pairs, unless there are at
 * least 256 and the keys span fewer than 2^22. */
#define RADIX_BITS 11
static bool radix_sort_ints(lc_kv *xs, lc_kv *tmp, int64_t n) {
    if (n < 256) return false;
    int64_t lo = xs[0].key.u.i, hi = lo;
    for (int64_t i = 1; i < n; i++) {
        int64_t k = xs[i].key.u.i;
        if (k < lo) lo = k;
        if (k > hi) hi = k;
    }
    uint64_t range = (uint64_t)hi - (uint64_t)lo;
    if (n < 4096 && range >> (2 * RADIX_BITS)) return false;
    const uint64_t mask = (1 << RADIX_BITS) - 1;
    lc_kv *src = xs, *dst = tmp;
    for (int shift = 0; shift < 64 && range >> shift; shift += RADIX_BITS) {
        int64_t at[1 << RADIX_BITS] = {0};
        for (int64_t i = 0; i < n; i++) at[((uint64_t)src[i].key.u.i - (uint64_t)lo) >> shift & mask]++;
        int64_t sum = 0;
        for (int d = 0; d < 1 << RADIX_BITS; d++) {
            int64_t c = at[d];
            at[d] = sum;
            sum += c;
        }
        for (int64_t i = 0; i < n; i++) dst[at[((uint64_t)src[i].key.u.i - (uint64_t)lo) >> shift & mask]++] = src[i];
        lc_kv *t = src;
        src = dst;
        dst = t;
    }
    if (src != xs) memcpy(xs, src, sizeof(lc_kv) * n);
    return true;
}

/* Sorts plain ints in place; equal ints are alike, so any order of them is
 * the stable one. */
static void sort_ints_with(int64_t *xs, int64_t *tmp, int64_t n) {
    if (n <= 16) {
        for (int64_t i = 1; i < n; i++) {
            int64_t x = xs[i];
            int64_t j = i;
            while (j > 0 && x < xs[j - 1]) {
                xs[j] = xs[j - 1];
                j--;
            }
            xs[j] = x;
        }
        return;
    }
    int64_t m = n / 2;
    sort_ints_with(xs, tmp, m);
    sort_ints_with(xs + m, tmp, n - m);
    int64_t i = 0, j = m, k = 0;
    while (i < m && j < n) tmp[k++] = xs[j] < xs[i] ? xs[j++] : xs[i++];
    while (i < m) tmp[k++] = xs[i++];
    while (j < n) tmp[k++] = xs[j++];
    memcpy(xs, tmp, sizeof(int64_t) * n);
}

void lc_sort_ints(int64_t *xs, int64_t n) {
    int64_t *tmp = xmalloc(sizeof(int64_t) * (n ? n : 1));
    sort_ints_with(xs, tmp, n);
    free(tmp);
}

/* Sorts pairs by key, stably. Returns false (with the kinds in a and b) when
 * two keys can't be compared. */
bool lc_sort_kv(lc_kv *xs, int64_t n, char *a, char *b) {
    lc_kv *tmp = xmalloc(sizeof(lc_kv) * (n ? n : 1));
    g_sort_ok = true;
    bool ints = true;
    for (int64_t i = 0; i < n && ints; i++) ints = xs[i].key.tag == T_INT;
    if (ints) {
        if (!radix_sort_ints(xs, tmp, n)) merge_sort_ints(xs, tmp, n);
        free(tmp);
        return true;
    }
    merge_sort(xs, tmp, n);
    free(tmp);
    if (!g_sort_ok) {
        strcpy(a, g_bad_a);
        strcpy(b, g_bad_b);
    }
    return g_sort_ok;
}

lc_kv *lc_pairs_new(int64_t n) { return xmalloc(sizeof(lc_kv) * (n ? n : 1)); }

void lc_pairs_free(lc_kv *kv, int64_t n) {
    for (int64_t i = 0; i < n; i++) {
        lc_release(kv[i].key);
        lc_release(kv[i].val);
    }
    free(kv);
}

lc_v lc_sorted_pairs(lc_kv *kv, int64_t n, bool by, const char *site) {
    char a[64], b[64];
    if (!lc_sort_kv(kv, n, a, b)) {
        if (by) lc_panic("E0301", site, "cannot sort: keys %s and %s are not comparable", a, b);
        lc_panic("E0301", site, "cannot sort: %s and %s are not comparable", a, b);
    }
    lc_v out = lc_list_new(n);
    for (int64_t i = 0; i < n; i++) {
        lc_vec_push(out, kv[i].val);
        lc_release(kv[i].key);
    }
    free(kv);
    return out;
}

lc_v lc_heap_sorted(lc_v h) {
    lc_vec *x = VEC(h);
    lc_kv *kv = xmalloc(sizeof(lc_kv) * (x->len ? x->len : 1));
    for (int64_t i = 0; i < x->len; i++) kv[i] = (lc_kv){x->boxed[i], x->boxed[i]};
    char a[64], b[64];
    lc_sort_kv(kv, x->len, a, b);
    lc_v out = lc_list_new(x->len);
    for (int64_t i = 0; i < x->len; i++) lc_vec_push(out, lc_retain(kv[i].val));
    free(kv);
    return out;
}

/* ----- display ----- */

const char *lc_kind(lc_v v, char *buf) {
    switch (v.tag) {
    case T_UNIT: return "()";
    case T_NONE: return "none";
    case T_BOOL: return "bool";
    case T_INT: return "int";
    case T_FLOAT: return "f64";
    case T_STR: return "str";
    case T_LIST: return "list";
    case T_TUPLE:
        sprintf(buf, "%lld-tuple", (long long)VEC(v)->len);
        return buf;
    case T_MAP: return "map";
    case T_SET: return "set";
    case T_HEAP: return "heap";
    case T_STRUCT: return lc_prog->structs[REC(v)->ty].name;
    case T_VARIANT: return lc_prog->enums[REC(v)->ty].name;
    case T_RANGE: return "range";
    case T_ERR: return "error";
    case T_FUNC: return "fn";
    }
    return "?";
}

/* The shortest digits that read back as `a` (a > 0, finite), and the
 * decimal exponent of the first digit. */
static int shortest(double a, char *digits, int *exp10) {
    char buf[64];
    for (int p = 0; p <= 17; p++) {
        snprintf(buf, sizeof buf, "%.*e", p, a);
        if (strtod(buf, NULL) == a || p == 17) break;
    }
    int n = 0;
    char *e = strchr(buf, 'e');
    for (char *c = buf; c < e; c++)
        if (*c >= '0' && *c <= '9') digits[n++] = *c;
    while (n > 1 && digits[n - 1] == '0') n--;
    digits[n] = 0;
    *exp10 = atoi(e + 1);
    return n;
}

/* Rust's `{:?}` for f64, which the interpreter prints. */
void lc_fmt_float(lc_buf *b, double f) {
    if (isinf(f)) {
        lc_buf_puts(b, f > 0 ? "inf" : "-inf");
        return;
    }
    if (isnan(f)) {
        lc_buf_puts(b, "nan");
        return;
    }
    if (f == 0) {
        lc_buf_puts(b, signbit(f) ? "-0.0" : "0.0");
        return;
    }
    if (f < 0) lc_buf_putc(b, '-');
    double a = fabs(f);
    char d[32];
    int e;
    int n = shortest(a, d, &e);
    if (a >= 1e-4 && a < 1e16) {
        if (e < 0) {
            lc_buf_puts(b, "0.");
            for (int i = 0; i < -e - 1; i++) lc_buf_putc(b, '0');
            lc_buf_put(b, d, n);
        } else if (e >= n - 1) {
            lc_buf_put(b, d, n);
            for (int i = 0; i < e - (n - 1); i++) lc_buf_putc(b, '0');
            lc_buf_puts(b, ".0");
        } else {
            lc_buf_put(b, d, e + 1);
            lc_buf_putc(b, '.');
            lc_buf_put(b, d + e + 1, n - e - 1);
        }
    } else {
        lc_buf_putc(b, d[0]);
        if (n > 1) {
            lc_buf_putc(b, '.');
            lc_buf_put(b, d + 1, n - 1);
        }
        char t[16];
        snprintf(t, sizeof t, "e%d", e);
        lc_buf_puts(b, t);
    }
}

/* Rust's `{}` for f64: no exponent, no `.0` on whole numbers. */
void lc_fmt_float_display(lc_buf *b, double f) {
    if (isinf(f) || isnan(f)) {
        lc_fmt_float(b, f);
        return;
    }
    if (f == 0) {
        lc_buf_puts(b, signbit(f) ? "-0" : "0");
        return;
    }
    if (f < 0) lc_buf_putc(b, '-');
    char d[32];
    int e;
    int n = shortest(fabs(f), d, &e);
    if (e < 0) {
        lc_buf_puts(b, "0.");
        for (int i = 0; i < -e - 1; i++) lc_buf_putc(b, '0');
        lc_buf_put(b, d, n);
    } else if (e >= n - 1) {
        lc_buf_put(b, d, n);
        for (int i = 0; i < e - (n - 1); i++) lc_buf_putc(b, '0');
    } else {
        lc_buf_put(b, d, e + 1);
        lc_buf_putc(b, '.');
        lc_buf_put(b, d + e + 1, n - e - 1);
    }
}

static void seq(lc_buf *b, const char *open, const char *close, lc_v *items, int64_t n) {
    lc_buf_puts(b, open);
    for (int64_t i = 0; i < n; i++) {
        if (i) lc_buf_puts(b, ", ");
        lc_buf_repr(b, items[i]);
    }
    lc_buf_puts(b, close);
}

static void seq_vec(lc_buf *b, const char *open, const char *close, lc_vec *x) {
    lc_buf_puts(b, open);
    for (int64_t i = 0; i < x->len; i++) {
        if (i) lc_buf_puts(b, ", ");
        lc_buf_repr(b, lc_vget(x, i));
    }
    lc_buf_puts(b, close);
}

void lc_buf_repr(lc_buf *b, lc_v v) {
    char t[64];
    switch (v.tag) {
    case T_UNIT: lc_buf_puts(b, "()"); break;
    case T_NONE: lc_buf_puts(b, "none"); break;
    case T_BOOL: lc_buf_puts(b, v.u.i ? "true" : "false"); break;
    case T_INT: {
        int n = lc_fmt_int(t, v.u.i);
        lc_buf_put(b, t, n);
        break;
    }
    case T_FLOAT: lc_fmt_float(b, v.u.f); break;
    case T_STR: {
        lc_str *s = STR(v);
        lc_buf_putc(b, '"');
        for (int64_t i = 0; i < s->len; i++) {
            char c = s->data[i];
            switch (c) {
            case '"': lc_buf_puts(b, "\\\""); break;
            case '\\': lc_buf_puts(b, "\\\\"); break;
            case '\n': lc_buf_puts(b, "\\n"); break;
            case '\t': lc_buf_puts(b, "\\t"); break;
            case '\r': lc_buf_puts(b, "\\r"); break;
            default: lc_buf_putc(b, c);
            }
        }
        lc_buf_putc(b, '"');
        break;
    }
    case T_LIST: seq_vec(b, "[", "]", VEC(v)); break;
    case T_TUPLE:
        seq_vec(b, "(", "", VEC(v));
        if (VEC(v)->len == 1) lc_buf_putc(b, ',');
        lc_buf_putc(b, ')');
        break;
    case T_SET: {
        lc_map *m = MAP(v);
        lc_buf_putc(b, '{');
        for (int64_t i = 0; i < m->len; i++) {
            if (i) lc_buf_puts(b, ", ");
            lc_buf_repr(b, m->e[i].k);
        }
        lc_buf_putc(b, '}');
        break;
    }
    case T_HEAP: {
        lc_v s = lc_heap_sorted(v);
        lc_buf_puts(b, "heap(");
        seq_vec(b, "[", "]", VEC(s));
        lc_buf_putc(b, ')');
        lc_release(s);
        break;
    }
    case T_MAP: {
        lc_map *m = MAP(v);
        lc_buf_putc(b, '{');
        for (int64_t i = 0; i < m->len; i++) {
            if (i) lc_buf_puts(b, ", ");
            lc_buf_repr(b, m->e[i].k);
            lc_buf_puts(b, ": ");
            lc_buf_repr(b, m->e[i].v);
        }
        lc_buf_putc(b, '}');
        break;
    }
    case T_STRUCT: {
        lc_rec *r = REC(v);
        const lc_struct_info *si = &lc_prog->structs[r->ty];
        lc_buf_puts(b, si->name);
        lc_buf_putc(b, '{');
        for (int64_t i = 0; i < r->n && i < si->nfields; i++) {
            if (i) lc_buf_puts(b, ", ");
            lc_buf_puts(b, si->field_names[i]);
            lc_buf_puts(b, ": ");
            lc_buf_repr(b, r->f[i]);
        }
        lc_buf_putc(b, '}');
        break;
    }
    case T_VARIANT: {
        lc_rec *r = REC(v);
        lc_buf_puts(b, lc_prog->enums[r->ty].variants[r->tag].name);
        if (r->n) seq(b, "(", ")", r->f, r->n);
        break;
    }
    case T_RANGE: {
        lc_range *r = RANGE(v);
        if (r->step == 1)
            snprintf(t, sizeof t, "%lld..%lld", (long long)r->start, (long long)r->end);
        else
            snprintf(t, sizeof t, "(%lld..%lld).step_by(%lld)", (long long)r->start, (long long)r->end, (long long)r->step);
        lc_buf_puts(b, t);
        break;
    }
    case T_ERR:
        lc_buf_puts(b, "err(");
        lc_buf_repr(b, ERRV(v)->payload);
        lc_buf_putc(b, ')');
        break;
    case T_FUNC: {
        lc_fn *f = FN(v);
        switch (f->kind) {
        case FN_USER:
            lc_buf_puts(b, "<fn ");
            lc_buf_puts(b, lc_prog->fn_names[f->id]);
            lc_buf_putc(b, '>');
            break;
        case FN_CLOSURE: lc_buf_puts(b, "<lambda>"); break;
        case FN_CTOR:
            lc_buf_puts(b, "<fn ");
            lc_buf_puts(b, lc_prog->enums[f->id].variants[f->tag].name);
            lc_buf_putc(b, '>');
            break;
        case FN_METHOD:
            lc_buf_puts(b, "<fn ");
            lc_buf_puts(b, lc_method_names[f->id]);
            lc_buf_putc(b, '>');
            break;
        case FN_CONV:
            lc_buf_puts(b, "<fn ");
            lc_buf_puts(b, lc_convs[f->id].name);
            lc_buf_putc(b, '>');
            break;
        }
        break;
    }
    }
}

void lc_buf_display(lc_buf *b, lc_v v) {
    if (v.tag == T_STR) {
        lc_buf_put(b, STR(v)->data, STR(v)->len);
    } else if (v.tag == T_ERR) {
        lc_buf_display(b, ERRV(v)->payload);
    } else {
        lc_buf_repr(b, v);
    }
}

lc_v lc_display(lc_v v) {
    if (v.tag == T_STR) return lc_retain(v);
    if (v.tag == T_INT) {
        char t[24];
        return lc_str_new(t, lc_fmt_int(t, v.u.i));
    }
    lc_buf b = {0};
    lc_buf_display(&b, v);
    return lc_buf_finish(&b);
}

lc_v lc_repr(lc_v v) {
    lc_buf b = {0};
    lc_buf_repr(&b, v);
    return lc_buf_finish(&b);
}

/* ----- output ----- */

static char out_buf[1 << 16];
static size_t out_len;

void lc_flush(void) {
    if (out_len) {
        fwrite(out_buf, 1, out_len, stdout);
        out_len = 0;
    }
    fflush(stdout);
}

void lc_out(const char *s, int64_t n) {
    if (out_len + n > sizeof out_buf) {
        fwrite(out_buf, 1, out_len, stdout);
        out_len = 0;
        if ((size_t)n > sizeof out_buf) {
            fwrite(s, 1, n, stdout);
            return;
        }
    }
    memcpy(out_buf + out_len, s, n);
    out_len += n;
}

/* ----- the call stack and panics ----- */

#define MAX_DEPTH 20000
lc_call_rec *lc_frames;
int64_t lc_frames_cap;
#define frames lc_frames

/* Room for one more frame, at most MAX_DEPTH in all. */
static void grow_frames(void) {
    if (lc_depth == lc_frames_cap) {
        int64_t cap = lc_frames_cap ? lc_frames_cap * 2 : 256;
        lc_frames_cap = cap < MAX_DEPTH ? cap : MAX_DEPTH;
        lc_frames = xrealloc(lc_frames, sizeof(lc_call_rec) * lc_frames_cap);
    }
}

void lc_enter_slow(int fn_id) {
    if (lc_depth >= MAX_DEPTH)
        lc_panic("E0408", lc_callsite ? lc_callsite : "", "stack overflow: recursion deeper than %d calls (in `%s`)", MAX_DEPTH, lc_prog->fn_names[fn_id]);
    grow_frames();
    lc_frames[lc_depth++] = (lc_call_rec){fn_id, lc_callsite};
}

void lc_enter_lambda(const char *site) {
    if (lc_depth >= MAX_DEPTH) lc_panic("E0408", site, "stack overflow");
    grow_frames();
    /* No site: traces leave lambdas out. */
    lc_frames[lc_depth++] = (lc_call_rec){-1, NULL};
}

_Noreturn void lc_exit(int code) {
    lc_flush();
    fflush(stderr);
    exit(code);
}

_Noreturn void lc_panic(const char *code, const char *site, const char *fmt, ...) {
    lc_flush();
    va_list ap;
    va_start(ap, fmt);
    fprintf(stderr, "%s %s ", code, site);
    vfprintf(stderr, fmt, ap);
    fputc('\n', stderr);
    va_end(ap);
    /* Enclosing calls, innermost first, runs of one frame collapsed. Frames
     * without a site (lambdas, `main`) are left out, as the interpreter
     * leaves them out. */
    int shown = 0, total = 0, count = 0, fn = -1;
    const char *at = NULL;
    for (int64_t i = lc_depth - 1; i >= -1; i--) {
        if (i >= 0 && !frames[i].site) continue;
        if (i >= 0 && count > 0 && frames[i].fn == fn && frames[i].site == at) {
            count++;
            continue;
        }
        if (count > 0) {
            total++;
            if (shown < 4) {
                if (count > 1)
                    fprintf(stderr, "  in `%s`, called at %s (%d times)\n", lc_prog->fn_names[fn], at, count);
                else
                    fprintf(stderr, "  in `%s`, called at %s\n", lc_prog->fn_names[fn], at);
                shown++;
            }
        }
        if (i >= 0) {
            fn = frames[i].fn;
            at = frames[i].site;
            count = 1;
        }
    }
    if (total > 4) fprintf(stderr, "  ... %d more frames\n", total - 4);
    lc_exit(1);
}

/* ----- entry point ----- */

int lc_argc;
char **lc_argv;
static void (*g_init)(void);
static lc_v (*g_main)(lc_v *);
static int g_status;

static void *run_main(void *unused) {
    (void)unused;
    g_init();
    lc_v r = g_main(NULL);
    if (r.tag == T_ERR) {
        lc_flush();
        lc_buf b = {0};
        lc_buf_display(&b, r);
        fprintf(stderr, "error: %s\n", b.p ? b.p : "");
        g_status = 1;
    }
    lc_flush();
    return NULL;
}

int lc_main(int argc, char **argv, const lc_program *prog, void (*init)(void), lc_v (*main_fn)(lc_v *)) {
    lc_prog = prog;
    lc_argc = argc;
    lc_argv = argv;
    g_init = init;
    g_main = main_fn;
    /* Deep recursion needs a big stack, as the interpreter has. */
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, (size_t)1 << 30);
    pthread_t t;
    if (pthread_create(&t, &attr, run_main, NULL) != 0) {
        run_main(NULL);
    } else {
        pthread_join(t, NULL);
    }
    lc_flush();
    return g_status;
}
