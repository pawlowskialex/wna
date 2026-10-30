package wnatest

import java.util.concurrent.atomic.AtomicLong
import wna.WitError
import wna.test.sink.Listener
import wna.test.sink.exports.Sink
import wna.test.types.Bag
import wna.test.types.Failure
import wna.test.types.FailureException
import wna.test.types.Scalars
import wna.test.types.Shape
import wna.test.types.Small
import wna.test.types.Wide
import wna.test.types.exports.Counter
import wna.test.types.exports.Types
import wna.Some

/** `wna:test/types` in Kotlin: the same behaviour as `tests/conformance/rust-server`. */
class Server : Types {
    private val total = AtomicLong()

    inner class MyCounter(start: Long) : Counter {
        private val v = AtomicLong(start)

        init {
            total.addAndGet(start)
        }

        override fun add(n: Long): Long {
            total.addAndGet(n)
            return v.addAndGet(n)
        }

        override fun get(): Long = v.get()

        override fun merge(other: Counter) {
            add(other.get())
        }
    }

    override fun echoScalars(s: Scalars) = s
    override fun echoString(s: String) = s
    override fun echoBytes(b: ByteArray) = b
    override fun echoBag(b: Bag) = b
    override fun echoBags(b: List<Bag>) = b
    override fun echoShape(s: Shape) = s
    override fun echoOptions(a: String?, b: Some<UInt?>?) = a to b
    override fun echoFlags(a: Small, b: Wide) = a to b

    override fun check(n: ULong): ULong = when {
        n % 2uL == 0uL -> n
        n > 1000uL -> throw FailureException(Failure.TooBig(n))
        else -> throw FailureException(Failure.BadInput("$n is odd"))
    }

    override fun checkUnit(fail: Boolean) {
        if (fail) throw WitError(null)
    }

    override fun noResult(n: UInt) {}
    override fun boom(msg: String): UInt = throw IllegalStateException(msg)
    override fun sum(xs: List<Long>) = xs.sum()
    override fun concat(xs: List<String>) = xs.joinToString("")
    override fun newCounter(start: Long): Counter = MyCounter(start)
    override fun counterTotal() = total.get()
    override fun makeCounters(n: UInt): List<Counter> = List(n.toInt()) { MyCounter(it.toLong()) }
    override fun consume(c: Counter) = c.get()

    override fun emit(to: Listener, events: List<String>): UInt {
        for (e in events) to.onEvent(e)
        return to.count()
    }
}

/** `wna:test/sink-provider` in Kotlin: listeners that count events. */
class KotlinSink : Sink {
    class Recorder : wna.test.sink.exports.Listener {
        private val events = java.util.Collections.synchronizedList(ArrayList<String>())

        override fun onEvent(e: String) {
            events.add(e)
        }

        override fun count(): UInt = events.size.toUInt()
    }

    override fun newListener(): wna.test.sink.exports.Listener = Recorder()
}
