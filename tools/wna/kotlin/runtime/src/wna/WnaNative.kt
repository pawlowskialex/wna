package wna

import java.nio.ByteBuffer

/**
 * The generic JNI shim (`libwna_jni`, `tools/wna/kotlin/jni/wna_jni.c`).
 * Every table slot has one C shape, so these few natives serve every
 * interface. Addresses are `Long`s; strings cross as UTF-8 bytes.
 */
public object WnaNative {
    init {
        System.loadLibrary(System.getProperty("wna.jni.library", "wna_jni"))
    }

    /** Calls the slot at byte [offset] of [table], passing a malloc-based allocator for results. */
    @JvmStatic public external fun call(table: Long, inst: Long, offset: Int, args: Long, ret: Long): Int

    /** `header.size` of a table (0 for null). */
    @JvmStatic public external fun tableSize(table: Long): Int

    /** The exporter's message for the last trap on this thread. */
    @JvmStatic public external fun lastTrap(table: Long, inst: Long): ByteArray

    @JvmStatic public external fun malloc(size: Long): Long

    @JvmStatic public external fun free(ptr: Long)

    /** A direct buffer over native memory; set the byte order before use. */
    @JvmStatic public external fun view(addr: Long, len: Int): ByteBuffer

    /** Copies `len` bytes out of native memory. */
    @JvmStatic public external fun read(addr: Long, len: Int): ByteArray

    /** Copies `src` into native memory at `addr`. */
    @JvmStatic public external fun write(addr: Long, src: ByteArray)

    /** Allocates through a caller's `wna_alloc` (export results). Returns 0 when out of memory. */
    @JvmStatic public external fun allocViaAlloc(alloc: Long, size: Long, align: Long): Long

    @JvmStatic public external fun freeViaAlloc(alloc: Long, ptr: Long, size: Long, align: Long)

    /** A table of `nSlots` trampolines that dispatch to [dispatch]. Tables are shared per slot count. */
    @JvmStatic public external fun makeTable(nSlots: Int): Long

    /** Sets the message the shim's `last_trap` returns on this thread. */
    @JvmStatic public external fun setLastTrap(msg: ByteArray)

    /** Called by the shim's trampolines. Never throws. */
    @JvmStatic
    public fun dispatch(objId: Long, slot: Int, args: Long, ret: Long, alloc: Long): Int =
        Exports.dispatch(objId, slot, args, ret, alloc)
}
