package wnatest

import wna.Iface
import wna.WnaNative

/** The Rust side of the tests (`tests/conformance/kotlin/fixture`). */
object Fixture {
    init {
        System.loadLibrary("wna_test_kotlin_fixture")
    }

    @JvmStatic private external fun rustServer(out: Long): Int

    @JvmStatic private external fun runRustClient(typesTable: Long, typesInst: Long, sinkTable: Long, sinkInst: Long, msg: Long): Int

    /** The Rust server's `types` and the Rust sink it imports. */
    fun rustServer(): Pair<Iface, Iface> {
        val out = WnaNative.malloc(32)
        try {
            val st = rustServer(out)
            check(st == 0) { "instantiating the Rust server: status $st" }
            val b = wna.Lift.view(out, 32)
            return Iface(b.getLong(0), b.getLong(8)) to Iface(b.getLong(16), b.getLong(24))
        } finally {
            WnaNative.free(out)
        }
    }

    /** Runs the Rust client suite against [types] and [sink]; throws with its panic message on failure. */
    fun runRustClient(types: Iface, sink: Iface) {
        val msg = WnaNative.malloc(16)
        try {
            wna.Lift.view(msg, 16).putLong(0, 0L).putLong(8, 0L)
            if (runRustClient(types.table, types.inst, sink.table, sink.inst, msg) != 0) {
                val b = wna.Lift.view(msg, 16)
                val (p, n) = b.getLong(0) to b.getLong(8)
                val text = if (p == 0L) "?" else String(WnaNative.read(p, n.toInt()), Charsets.UTF_8)
                if (p != 0L) WnaNative.free(p)
                throw AssertionError("Rust client suite failed: $text")
            }
        } finally {
            WnaNative.free(msg)
        }
    }
}
