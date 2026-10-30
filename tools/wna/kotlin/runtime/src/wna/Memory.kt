package wna

import java.nio.ByteBuffer
import java.nio.ByteOrder

/** Native memory at [addr], written through [buf] starting at index [off]. */
public class Block(@JvmField public val addr: Long, @JvmField public val buf: ByteBuffer, @JvmField public val off: Int)

private val NATIVE: ByteOrder = ByteOrder.nativeOrder()

internal fun view(addr: Long, size: Int): ByteBuffer = WnaNative.view(addr, size).order(NATIVE)

private fun bytesFor(n: Int, size: Int): Int =
    try {
        Math.multiplyExact(n, size)
    } catch (e: ArithmeticException) {
        throw WnaTrap.invalid("list of $n elements is too large")
    }

/** Where lowered values go: a call's scratch [Arena] (imports) or the caller's allocator ([ResultLower], exports). */
public abstract class Lower {
    /** Allocates `size` bytes (`size > 0`). */
    public abstract fun alloc(size: Int, align: Int): Long

    public open fun block(size: Int, align: Int): Block {
        val p = alloc(size, align)
        return Block(p, view(p, size), 0)
    }

    public fun string(s: String, buf: ByteBuffer, at: Int) {
        bytes(s.encodeToByteArray(), buf, at)
    }

    public fun bytes(b: ByteArray, buf: ByteBuffer, at: Int) {
        val p = if (b.isEmpty()) 0L else alloc(b.size, 1).also { WnaNative.write(it, b) }
        buf.putLong(at, p)
        buf.putLong(at + 8, b.size.toLong())
    }

    /** Lowers a list: allocates the element array and calls [f] for each element's slot. */
    public inline fun <T> list(xs: List<T>, size: Int, align: Int, buf: ByteBuffer, at: Int, f: (T, ByteBuffer, Int) -> Unit) {
        val n = xs.size
        if (n == 0) {
            buf.putLong(at, 0L)
            buf.putLong(at + 8, 0L)
            return
        }
        val b = block(listBytes(n, size), align)
        for (i in 0 until n) f(xs[i], b.buf, b.off + i * size)
        buf.putLong(at, b.addr)
        buf.putLong(at + 8, n.toLong())
    }

    @PublishedApi internal fun listBytes(n: Int, size: Int): Int = bytesFor(n, size)

    public companion object {
        /** A `char`: a Unicode scalar value. */
        @JvmStatic
        public fun char(c: Int): Int {
            if (c < 0 || c > 0x10FFFF || (c in 0xD800..0xDFFF)) throw WnaTrap.invalid("invalid char 0x" + Integer.toHexString(c))
            return c
        }
    }
}

/**
 * Per-call scratch memory for an import's arguments and result struct:
 * bump-allocated from malloc'd chunks, released after the call. Arenas are
 * pooled per thread (a stack, so reentrant calls get their own).
 */
public class Arena private constructor() : Lower() {
    private class Chunk(val addr: Long, val size: Int, val buf: ByteBuffer)

    private var first: Chunk? = null
    private var cur: Chunk? = null
    private var used = 0
    private val extra = ArrayList<Chunk>()

    private fun chunk(size: Int): Chunk {
        val p = WnaNative.malloc(size.toLong())
        if (p == 0L) throw WnaTrap(WnaTrap.OOM, "out of memory")
        return Chunk(p, size, view(p, size))
    }

    override fun alloc(size: Int, align: Int): Long {
        val b = block(size, align)
        return b.addr
    }

    override fun block(size: Int, align: Int): Block {
        var c = cur
        var off = if (c == null) 0 else (used + align - 1) and (align - 1).inv()
        if (c == null || off + size > c.size) {
            c = if (size > CHUNK / 2) chunk(size) else if (first == null) chunk(CHUNK).also { first = it } else chunk(CHUNK)
            if (c !== first) extra.add(c)
            off = 0
            if (size <= CHUNK / 2) {
                cur = c
                used = size
            }
        } else {
            used = off + size
        }
        return Block(c.addr + off, c.buf, off)
    }

    /** Frees everything but the first chunk and returns the arena to this thread's pool. */
    public fun release() {
        for (c in extra) WnaNative.free(c.addr)
        extra.clear()
        cur = first
        used = 0
        val pool = POOL.get()
        if (pool.size < 8) pool.addLast(this) else first?.let { WnaNative.free(it.addr) }
    }

    public companion object {
        private const val CHUNK = 16 * 1024
        private val POOL = ThreadLocal.withInitial { ArrayDeque<Arena>() }

        @JvmStatic
        public fun acquire(): Arena = POOL.get().removeLastOrNull() ?: Arena()
    }
}

/**
 * An export's result buffers, allocated through the caller's `wna_alloc`.
 * If lowering fails, [rollback] frees what was already allocated; otherwise
 * ownership passes to the caller.
 */
public class ResultLower(private val alloc: Long) : Lower() {
    private val live = ArrayList<LongArray>()

    override fun alloc(size: Int, align: Int): Long {
        val p = WnaNative.allocViaAlloc(alloc, size.toLong(), align.toLong())
        if (p == 0L) throw WnaTrap(WnaTrap.OOM, "out of memory")
        live.add(longArrayOf(p, size.toLong(), align.toLong()))
        return p
    }

    public fun rollback() {
        for (a in live) WnaNative.freeViaAlloc(alloc, a[0], a[1], a[2])
        live.clear()
    }
}

/**
 * Reads values out of native memory. An importer lifts with `owned = true`:
 * the result's buffers came from the shim's malloc allocator and are freed
 * by [finish]. An exporter lifts parameters (views valid for the call) with
 * `owned = false`.
 */
public class Lift(private val owned: Boolean) {
    private val toFree = ArrayList<Long>()

    public fun string(buf: ByteBuffer, at: Int): String = utf8(bytes(buf, at))

    public fun bytes(buf: ByteBuffer, at: Int): ByteArray {
        val p = buf.getLong(at)
        val n = length(buf.getLong(at + 8), 1)
        adopt(p)
        return if (n == 0) EMPTY else WnaNative.read(p, n)
    }

    public inline fun <T> list(buf: ByteBuffer, at: Int, size: Int, f: (ByteBuffer, Int) -> T): List<T> {
        val p = buf.getLong(at)
        val n = length(buf.getLong(at + 8), size)
        adopt(p)
        if (n == 0) return emptyList()
        val v = elements(p, n * size)
        return List(n) { f(v, it * size) }
    }

    @PublishedApi internal fun elements(p: Long, size: Int): ByteBuffer = view(p, size)

    @PublishedApi
    internal fun length(n: Long, size: Int): Int {
        if (n < 0 || n > Int.MAX_VALUE / size) throw WnaTrap.invalid("list length $n is too large")
        return n.toInt()
    }

    @PublishedApi
    internal fun adopt(p: Long) {
        if (owned && p != 0L) toFree.add(p)
    }

    /** Frees adopted buffers. */
    public fun finish() {
        for (p in toFree) WnaNative.free(p)
        toFree.clear()
    }

    public companion object {
        private val EMPTY = ByteArray(0)

        /** A view of an export's argument or result struct. */
        @JvmStatic
        public fun view(addr: Long, size: Int): ByteBuffer = wna.view(addr, size)

        /** Lifts an import's result, then frees its buffers. */
        public inline fun <R> result(f: (Lift) -> R): R {
            val lx = Lift(true)
            try {
                return f(lx)
            } finally {
                lx.finish()
            }
        }

        @JvmStatic
        public fun char(c: Int): Int = Lower.char(c)

        /** An `option`/`result` discriminant: 0 or 1. */
        @JvmStatic
        public fun bit(d: Byte, what: String): Boolean = when (d.toInt()) {
            0 -> false
            1 -> true
            else -> throw WnaTrap.invalid("$what discriminant $d")
        }

        @JvmStatic
        public fun <E> case(entries: List<E>, d: Int, what: String): E =
            entries.getOrNull(d) ?: throw WnaTrap.invalid("$what: case $d")

        /** Strict UTF-8: invalid bytes are a [WnaTrap.INVALID] trap, as in every other binding. */
        @JvmStatic
        public fun utf8(b: ByteArray): String {
            val s = String(b, Charsets.UTF_8)
            if (s.indexOf('�') >= 0) {
                try {
                    Charsets.UTF_8.newDecoder().decode(ByteBuffer.wrap(b))
                } catch (e: java.nio.charset.CharacterCodingException) {
                    throw WnaTrap.invalid("string is not valid UTF-8")
                }
            }
            return s
        }
    }
}
