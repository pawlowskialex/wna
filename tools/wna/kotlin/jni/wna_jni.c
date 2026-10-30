/* The generic JNI shim behind every Kotlin WNA binding.
 *
 * Every table slot has one C shape, so this fixed library serves every
 * interface; generated Kotlin does all layout work over direct ByteBuffers.
 *
 * - Imports: `WnaNative.call` invokes a slot (with the header size check of
 *   `wna_call`) passing a malloc-based `wna_alloc`, so result buffers are
 *   freed with `WnaNative.free`.
 * - Exports: a pool of 256 trampolines, one per slot index. A table built by
 *   `WnaNative.makeTable(n)` holds trampolines 0..n-1; the instance pointer
 *   is the id of a Kotlin `wna.Export`. A trampoline attaches the thread to
 *   the JVM if needed and calls `WnaNative.dispatch(id, slot, args, ret,
 *   alloc)`, which never throws; the trap message travels through a
 *   thread-local buffer read by the table's `last_trap`.
 *
 * Strings cross as UTF-8 byte arrays, never as JNI modified UTF-8.
 */
#include <jni.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "wna.h"

#define N_TRAMPOLINES 256

/* Android declares the env out-parameter as JNIEnv**, the JDK as void**. */
#ifdef __ANDROID__
#define ENV_OUT(e) (&(e))
#else
#define ENV_OUT(e) ((void **)&(e))
#endif

#define PTR(x) ((void *)(intptr_t)(x))
#define JLONG(p) ((jlong)(intptr_t)(p))

static JavaVM *g_vm;
static jclass g_native;
static jmethodID g_dispatch;
static pthread_key_t g_attached;

/* Threads the shim attached are detached when they exit. */
static void detach(void *unused) {
  (void)unused;
  (*g_vm)->DetachCurrentThread(g_vm);
}

JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM *vm, void *reserved) {
  (void)reserved;
  JNIEnv *env;
  if ((*vm)->GetEnv(vm, (void **)&env, JNI_VERSION_1_6) != JNI_OK) return JNI_ERR;
  jclass c = (*env)->FindClass(env, "wna/WnaNative");
  if (c == NULL) return JNI_ERR;
  g_native = (jclass)(*env)->NewGlobalRef(env, c);
  g_dispatch = (*env)->GetStaticMethodID(env, c, "dispatch", "(JIJJJ)I");
  if (g_native == NULL || g_dispatch == NULL) return JNI_ERR;
  if (pthread_key_create(&g_attached, detach) != 0) return JNI_ERR;
  g_vm = vm;
  return JNI_VERSION_1_6;
}

/* ---- Trap messages of exports (per thread) ---------------------------- */

static _Thread_local wna_string g_last_trap;

static void set_last_trap(const uint8_t *msg, size_t len) {
  uint8_t *p = realloc(g_last_trap.ptr, len ? len : 1);
  if (p == NULL) {
    g_last_trap.len = 0;
    return;
  }
  if (len) memcpy(p, msg, len);
  g_last_trap.ptr = p;
  g_last_trap.len = len;
}

static void last_trap(void *inst, wna_string *msg) {
  (void)inst;
  *msg = g_last_trap;
}

/* ---- Trampolines ------------------------------------------------------ */

static int32_t dispatch(int32_t slot, void *inst, const void *args, void *ret, const wna_alloc *alloc) {
  JNIEnv *env;
  jint st = (*g_vm)->GetEnv(g_vm, (void **)&env, JNI_VERSION_1_6);
  if (st == JNI_EDETACHED) {
    if ((*g_vm)->AttachCurrentThreadAsDaemon(g_vm, ENV_OUT(env), NULL) != JNI_OK) {
      static const char m[] = "cannot attach thread to the JVM";
      set_last_trap((const uint8_t *)m, sizeof m - 1);
      return WNA_TRAP_WRONG_THREAD;
    }
    pthread_setspecific(g_attached, env);
  } else if (st != JNI_OK) {
    return WNA_TRAP_WRONG_THREAD;
  }
  jint code = (*env)->CallStaticIntMethod(env, g_native, g_dispatch, JLONG(inst), (jint)slot, JLONG(args), JLONG(ret), JLONG(alloc));
  if ((*env)->ExceptionCheck(env)) {
    /* The dispatcher catches every Throwable; this is a VM-level failure. */
    (*env)->ExceptionClear(env);
    static const char m[] = "uncaught JVM exception in export dispatch";
    set_last_trap((const uint8_t *)m, sizeof m - 1);
    return WNA_TRAP_PANIC;
  }
  return code;
}

#define TRAMP(a, b, c, d)                                                                         \
  static int32_t tramp_##a##b##c##d(void *inst, const void *args, void *ret, const wna_alloc *al) { \
    return dispatch(a * 64 + b * 16 + c * 4 + d, inst, args, ret, al);                            \
  }
#define ENTRY(a, b, c, d) tramp_##a##b##c##d,
#define X4(M, a, b, c) M(a, b, c, 0) M(a, b, c, 1) M(a, b, c, 2) M(a, b, c, 3)
#define X16(M, a, b) X4(M, a, b, 0) X4(M, a, b, 1) X4(M, a, b, 2) X4(M, a, b, 3)
#define X64(M, a) X16(M, a, 0) X16(M, a, 1) X16(M, a, 2) X16(M, a, 3)
#define X256(M) X64(M, 0) X64(M, 1) X64(M, 2) X64(M, 3)

X256(TRAMP)
static const wna_fn g_trampolines[N_TRAMPOLINES] = {X256(ENTRY)};

/* Tables depend only on the slot count, so one per count is shared. */
static void *g_tables[N_TRAMPOLINES + 1];
static pthread_mutex_t g_tables_lock = PTHREAD_MUTEX_INITIALIZER;

JNIEXPORT jlong JNICALL Java_wna_WnaNative_makeTable(JNIEnv *env, jclass cls, jint n) {
  (void)env, (void)cls;
  if (n < 0 || n > N_TRAMPOLINES) return 0;
  pthread_mutex_lock(&g_tables_lock);
  void *t = g_tables[n];
  if (t == NULL) {
    size_t size = sizeof(wna_table_header) + sizeof(wna_fn) * (size_t)n;
    t = malloc(size);
    if (t != NULL) {
      wna_table_header *h = t;
      h->size = (uint32_t)size;
      h->reserved = 0;
      h->last_trap = last_trap;
      memcpy((char *)t + sizeof(wna_table_header), g_trampolines, sizeof(wna_fn) * (size_t)n);
      g_tables[n] = t;
    }
  }
  pthread_mutex_unlock(&g_tables_lock);
  return JLONG(t);
}

JNIEXPORT void JNICALL Java_wna_WnaNative_setLastTrap(JNIEnv *env, jclass cls, jbyteArray msg) {
  (void)cls;
  jsize n = (*env)->GetArrayLength(env, msg);
  jbyte *b = (*env)->GetByteArrayElements(env, msg, NULL);
  if (b == NULL) return;
  set_last_trap((const uint8_t *)b, (size_t)n);
  (*env)->ReleaseByteArrayElements(env, msg, b, JNI_ABORT);
}

/* ---- Imports ---------------------------------------------------------- */

JNIEXPORT jint JNICALL Java_wna_WnaNative_call(JNIEnv *env, jclass cls, jlong table, jlong inst, jint offset, jlong args, jlong ret) {
  (void)env, (void)cls;
  wna_iface i = {PTR(table), PTR(inst)};
  return wna_call(i, (size_t)offset, PTR(args), PTR(ret), wna_libc_alloc());
}

JNIEXPORT jint JNICALL Java_wna_WnaNative_tableSize(JNIEnv *env, jclass cls, jlong table) {
  (void)env, (void)cls;
  const wna_table_header *h = PTR(table);
  return h == NULL ? 0 : (jint)h->size;
}

static jbyteArray bytes_of(JNIEnv *env, const void *p, size_t len) {
  jbyteArray a = (*env)->NewByteArray(env, (jsize)len);
  if (a != NULL && len != 0) (*env)->SetByteArrayRegion(env, a, 0, (jsize)len, (const jbyte *)p);
  return a;
}

JNIEXPORT jbyteArray JNICALL Java_wna_WnaNative_lastTrap(JNIEnv *env, jclass cls, jlong table, jlong inst) {
  (void)cls;
  const wna_table_header *h = PTR(table);
  wna_string msg = {NULL, 0};
  if (h != NULL && h->last_trap != NULL) h->last_trap(PTR(inst), &msg);
  return bytes_of(env, msg.ptr, msg.ptr == NULL ? 0 : msg.len);
}

/* ---- Memory ----------------------------------------------------------- */

JNIEXPORT jlong JNICALL Java_wna_WnaNative_malloc(JNIEnv *env, jclass cls, jlong size) {
  (void)env, (void)cls;
  return JLONG(malloc(size > 0 ? (size_t)size : 1));
}

JNIEXPORT void JNICALL Java_wna_WnaNative_free(JNIEnv *env, jclass cls, jlong p) {
  (void)env, (void)cls;
  free(PTR(p));
}

JNIEXPORT jobject JNICALL Java_wna_WnaNative_view(JNIEnv *env, jclass cls, jlong addr, jint len) {
  (void)cls;
  return (*env)->NewDirectByteBuffer(env, PTR(addr), (jlong)len);
}

JNIEXPORT jbyteArray JNICALL Java_wna_WnaNative_read(JNIEnv *env, jclass cls, jlong addr, jint len) {
  (void)cls;
  return bytes_of(env, PTR(addr), (size_t)len);
}

JNIEXPORT void JNICALL Java_wna_WnaNative_write(JNIEnv *env, jclass cls, jlong addr, jbyteArray src) {
  (void)cls;
  jsize n = (*env)->GetArrayLength(env, src);
  if (n != 0) (*env)->GetByteArrayRegion(env, src, 0, n, (jbyte *)PTR(addr));
}

/* An export's result buffers come from the caller's allocator. */
JNIEXPORT jlong JNICALL Java_wna_WnaNative_allocViaAlloc(JNIEnv *env, jclass cls, jlong alloc, jlong size, jlong align) {
  (void)env, (void)cls;
  const wna_alloc *a = PTR(alloc);
  return JLONG(a->realloc(a->ctx, NULL, 0, (size_t)align, (size_t)size));
}

JNIEXPORT void JNICALL Java_wna_WnaNative_freeViaAlloc(JNIEnv *env, jclass cls, jlong alloc, jlong p, jlong size, jlong align) {
  (void)env, (void)cls;
  const wna_alloc *a = PTR(alloc);
  a->realloc(a->ctx, PTR(p), (size_t)size, (size_t)align, 0);
}
