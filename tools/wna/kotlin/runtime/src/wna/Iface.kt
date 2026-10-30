package wna

/** `wna_iface`: a function table and the exporter's instance pointer. */
public data class Iface(public val table: Long, public val inst: Long) {
    /** Calls the slot at byte [offset]; throws [WnaTrap] on a nonzero status. [what] names the function in messages. */
    public fun call(offset: Int, args: Long, ret: Long, what: String) {
        val st = WnaNative.call(table, inst, offset, args, ret)
        if (st != WnaTrap.OK) throw trap(st, offset, what)
    }

    /** Calls a resource's drop slot. A drop never throws: a failure here has no one to report to. */
    public fun drop(offset: Int, handle: Int) {
        val cx = Arena.acquire()
        try {
            val a = cx.block(4, 4)
            a.buf.putInt(a.off, handle)
            WnaNative.call(table, inst, offset, a.addr, 0L)
        } finally {
            cx.release()
        }
    }

    private fun trap(st: Int, offset: Int, what: String): WnaTrap {
        val msg = when {
            table == 0L -> "interface not bound"
            st == WnaTrap.UNAVAILABLE && WnaNative.tableSize(table) < offset + 8 -> "the exporter's table predates this function"
            else -> String(WnaNative.lastTrap(table, inst), Charsets.UTF_8)
        }
        return WnaTrap(st, if (msg.isEmpty()) "$what: ${WnaTrap.name(st)}" else "$what: $msg")
    }
}
