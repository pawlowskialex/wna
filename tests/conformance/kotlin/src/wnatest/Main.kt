package wnatest

import kotlin.system.exitProcess
import wna.Iface
import wna.WnaNative
import wna.WnaTrap
import wna.test.sink.Sink
import wna.test.sink.exports.SinkExport
import wna.test.types.Types
import wna.test.types.exports.TypesExport

private var failures = 0

private fun scenario(name: String, f: () -> Unit) {
    try {
        f()
        println("ok   $name")
    } catch (t: Throwable) {
        failures++
        println("FAIL $name: $t")
        t.printStackTrace()
    }
}

/** Runs the Kotlin binding against the Rust reference and itself; exits nonzero on any failure. */
fun main() {
    val (rustTypes, rustSink) = Fixture.rustServer()
    val kotlinSink = SinkExport(KotlinSink())
    val kotlinTypes = TypesExport(Server(), Sink(kotlinSink.iface))

    scenario("kotlin client -> rust server (rust sink)") {
        Client.runSuite(Types(rustTypes), Sink(rustSink))
    }
    scenario("kotlin client -> kotlin server (kotlin sink)") {
        Client.runSuite(Types(kotlinTypes.iface), Sink(kotlinSink.iface))
        check(kotlinTypes.counterHandles.size == 0) { "counters leaked: ${kotlinTypes.counterHandles.size}" }
    }
    scenario("rust client (native thread) -> kotlin server (kotlin sink)") {
        Fixture.runRustClient(kotlinTypes.iface, kotlinSink.iface)
        check(kotlinTypes.counterHandles.size == 0) { "counters leaked: ${kotlinTypes.counterHandles.size}" }
    }
    scenario("kotlin client, 8 threads -> rust server") {
        val errors = java.util.concurrent.ConcurrentLinkedQueue<Throwable>()
        val threads = List(8) { n ->
            Thread {
                try {
                    val t = Types(rustTypes)
                    repeat(200) { i -> Client.eq(t.echoBag(Client.bag((n + i) % 7)), Client.bag((n + i) % 7), "echoBag") }
                } catch (e: Throwable) {
                    errors.add(e)
                }
            }.apply { start() }
        }
        threads.forEach { it.join() }
        errors.peek()?.let { throw it }
    }
    scenario("a table that predates a function traps UNAVAILABLE") {
        val t = Types(Iface(WnaNative.makeTable(0), 1))
        val e = Client.trap { t.noResult(1u) }
        Client.eq(e.code, WnaTrap.UNAVAILABLE, "code")
        check(e.message.contains("predates")) { e.message }
    }
    scenario("a closed export traps UNAVAILABLE") {
        val closed = TypesExport(Server(), Sink(kotlinSink.iface))
        val iface = closed.iface
        closed.close()
        Client.eq(Client.trap { Types(iface).noResult(1u) }.code, WnaTrap.UNAVAILABLE, "code")
    }

    println(if (failures == 0) "all Kotlin conformance scenarios passed" else "$failures scenario(s) failed")
    exitProcess(if (failures == 0) 0 else 1)
}
