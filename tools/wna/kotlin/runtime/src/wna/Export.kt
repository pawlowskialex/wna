package wna

import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong

/**
 * An interface implemented in Kotlin and exposed as a WNA table. Generated
 * `<Iface>Export` classes implement [call]; [iface] is what native callers
 * receive. The table's instance pointer is this object's registry id.
 */
public abstract class Export(private val nSlots: Int) : AutoCloseable {
    init {
        require(nSlots in 0..256) { "an exported interface has at most 256 slots" }
    }

    /** The table native code calls. Registers this export on first use. */
    public val iface: Iface by lazy {
        val table = WnaNative.makeTable(nSlots)
        if (table == 0L) throw WnaTrap(WnaTrap.OOM, "cannot allocate a table")
        Iface(table, Exports.register(this))
    }

    /** Runs slot [slot]. Throwing [WnaTrap] reports its code; any other throwable is a panic. */
    public abstract fun call(slot: Int, args: Long, ret: Long, alloc: Long)

    /** Unregisters the table: later calls trap with `UNAVAILABLE`. */
    override fun close() {
        Exports.unregister(iface.inst)
    }
}

/** The registry the shim's trampolines dispatch through. */
public object Exports {
    private val next = AtomicLong(1)
    private val live = ConcurrentHashMap<Long, Export>()

    internal fun register(e: Export): Long {
        val id = next.getAndIncrement()
        live[id] = e
        return id
    }

    internal fun unregister(id: Long) {
        live.remove(id)
    }

    /** The edge: every throwable becomes a status, with its message for `last_trap`. */
    internal fun dispatch(objId: Long, slot: Int, args: Long, ret: Long, alloc: Long): Int {
        val (code, msg) = try {
            val e = live[objId] ?: throw WnaTrap(WnaTrap.UNAVAILABLE, "export $objId was closed")
            e.call(slot, args, ret, alloc)
            return WnaTrap.OK
        } catch (t: WnaTrap) {
            t.code to t.message
        } catch (t: OutOfMemoryError) {
            WnaTrap.OOM to "out of memory"
        } catch (t: StackOverflowError) {
            WnaTrap.PANIC to "stack overflow"
        } catch (t: Throwable) {
            WnaTrap.PANIC to (t.message ?: t.toString())
        }
        try {
            WnaNative.setLastTrap(msg.encodeToByteArray())
        } catch (_: Throwable) {
        }
        return code
    }
}

/**
 * Handles of resources this side exports. A handle is a nonzero `u32`: the
 * low 24 bits index a slot, the high 8 bits are its generation (1..255), so a
 * stale handle is a [WnaTrap.BAD_HANDLE] trap.
 */
public class HandleTable<T : Any> {
    private val values = ArrayList<T?>()
    private val generations = ArrayList<Int>()
    private val free = ArrayDeque<Int>()

    @Synchronized
    public fun insert(value: T): Int {
        val index = free.removeLastOrNull() ?: run {
            check(values.size <= INDEX_MASK) { "handle table full" }
            values.add(null)
            generations.add(0)
            values.size - 1
        }
        val g = generations[index]
        val gen = if (g == 255) 1 else g + 1
        generations[index] = gen
        values[index] = value
        return (gen shl INDEX_BITS) or index
    }

    @Synchronized
    public fun get(handle: Int): T {
        val index = handle and INDEX_MASK
        val v = values.getOrNull(index)
        if (v == null || generations[index] != handle ushr INDEX_BITS) throw WnaTrap.badHandle(handle)
        return v
    }

    @Synchronized
    public fun remove(handle: Int): T {
        val v = get(handle)
        val index = handle and INDEX_MASK
        values[index] = null
        free.addLast(index)
        return v
    }

    @get:Synchronized
    public val size: Int get() = values.size - free.size

    private companion object {
        const val INDEX_BITS = 24
        const val INDEX_MASK = (1 shl INDEX_BITS) - 1
    }
}
