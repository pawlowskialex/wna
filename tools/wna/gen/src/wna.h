/* WNA common definitions: the binary contract shared by every component.
 *
 * - A component exposes one symbol returning a `wna_component` descriptor.
 * - Every interface is reached through a `wna_iface`: a function table plus
 *   the exporter's instance pointer.
 * - Every table slot has one shape:
 *     int32_t f(void *inst, const ARGS *args, RET *ret, const wna_alloc *alloc)
 *   ARGS is a struct of the parameters (a method's `self` handle first), RET
 *   the result's layout; either is NULL when absent. The status is WNA_OK or
 *   a trap code. `ret` is written only on WNA_OK.
 * - Parameters are views valid for the call. Every string/list inside a
 *   result is allocated through the caller's `alloc` and owned by the caller.
 */
#ifndef WNA_H
#define WNA_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

#ifdef __cplusplus
extern "C" {
#endif

#define WNA_ABI_VERSION 1

#define WNA_OK 0
#define WNA_TRAP_PANIC 1
#define WNA_TRAP_BAD_HANDLE 2
#define WNA_TRAP_OOM 3
#define WNA_TRAP_INVALID 4
#define WNA_TRAP_REENTRANCE 5
#define WNA_TRAP_WRONG_THREAD 6
#define WNA_TRAP_UNAVAILABLE 7
#define WNA_CANCELLED 8

/* The caller's allocator. realloc(ctx, NULL, 0, align, n) allocates;
 * realloc(ctx, p, old, align, 0) frees and returns NULL. */
typedef struct wna_alloc {
  void *ctx;
  void *(*realloc)(void *ctx, void *old, size_t old_size, size_t align, size_t new_size);
} wna_alloc;

/* string: UTF-8 bytes. Lists use the same {ptr, len} shape with a typed ptr. */
typedef struct wna_string {
  uint8_t *ptr;
  size_t len;
} wna_string;

typedef struct wna_iface {
  const void *table;
  void *inst;
} wna_iface;

typedef struct wna_table_header {
  uint32_t size; /* byte size of the table, header included */
  uint32_t reserved;
  /* Message of the last trap returned to this thread by this instance; owned
   * by the callee and valid until the next call into it on this thread. */
  void (*last_trap)(void *inst, wna_string *msg);
} wna_table_header;

typedef int32_t (*wna_fn)(void *inst, const void *args, void *ret, const wna_alloc *alloc);

typedef struct wna_component {
  uint32_t abi_version; /* WNA_ABI_VERSION */
  const char *name;     /* "acme:core-rs" */
  uint32_t n_imports;
  const char *const *imports; /* "acme:shell/secrets@1.0.0", ... */
  uint32_t n_exports;
  const char *const *exports;
  /* `imports` has n_imports entries in descriptor order. */
  int32_t (*instantiate)(const wna_iface *imports, void **inst);
  /* `iface` is "ns:pkg/name" or "ns:pkg/name@x.y.z". */
  int32_t (*get_export)(void *inst, const char *iface, wna_iface *out);
  int32_t (*destroy)(void *inst);
} wna_component;

/* Calls the slot at byte `offset` of an interface table, or returns
 * WNA_TRAP_UNAVAILABLE when the exporter's table predates that slot. */
static inline int32_t wna_call(wna_iface i, size_t offset, const void *args, void *ret, const wna_alloc *alloc) {
  const wna_table_header *h = (const wna_table_header *)i.table;
  if (h == NULL || h->size < offset + sizeof(wna_fn)) return WNA_TRAP_UNAVAILABLE;
  wna_fn f = *(const wna_fn *)((const char *)i.table + offset);
  return f(i.inst, args, ret, alloc);
}

/* An allocator over malloc/realloc/free, for C, C++ and Swift callers.
 * WIT types never need more than 8-byte alignment, which malloc guarantees. */
static inline void *wna_libc_realloc(void *ctx, void *old, size_t old_size, size_t align, size_t new_size) {
  (void)ctx; (void)old_size; (void)align;
  if (new_size == 0) { free(old); return NULL; }
  return realloc(old, new_size);
}

static inline const wna_alloc *wna_libc_alloc(void) {
  static const wna_alloc a = {NULL, wna_libc_realloc};
  return &a;
}

static inline void wna_libc_free(void *p) { free(p); }

#ifdef __cplusplus
}
#endif
#endif /* WNA_H */
