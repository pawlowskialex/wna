package wnatest

import wna.Some
import wna.WitError
import wna.WnaTrap
import wna.test.sink.Sink
import wna.test.types.Bag
import wna.test.types.Color
import wna.test.types.Counter
import wna.test.types.Failure
import wna.test.types.FailureException
import wna.test.types.Scalars
import wna.test.types.Shape
import wna.test.types.Small
import wna.test.types.Types
import wna.test.types.Wide

/** The Kotlin port of `tests/conformance/rust-client`'s `run_suite`: every check, same values. */
object Client {
    fun <T> eq(actual: T, expected: T, what: String = "") {
        if (actual != expected) throw AssertionError("$what: expected <$expected>, got <$actual>")
    }

    fun trap(f: () -> Unit): WnaTrap {
        try {
            f()
        } catch (t: WnaTrap) {
            return t
        }
        throw AssertionError("expected a trap")
    }

    inline fun <reified E : Throwable> throws(f: () -> Unit): E {
        try {
            f()
        } catch (e: Throwable) {
            if (e is E) return e
            throw AssertionError("expected ${E::class.simpleName}, got $e", e)
        }
        throw AssertionError("expected ${E::class.simpleName}")
    }

    fun scalars(seed: Int): Scalars = Scalars(
        b = seed % 2 == 0,
        u8v = UByte.MAX_VALUE,
        u16v = (UShort.MAX_VALUE.toInt() - seed).toUShort(),
        u32v = UInt.MAX_VALUE,
        u64v = ULong.MAX_VALUE - seed.toULong(),
        s8v = Byte.MIN_VALUE,
        s16v = Short.MIN_VALUE,
        s32v = Int.MIN_VALUE + seed,
        s64v = Long.MIN_VALUE,
        f32v = java.lang.Float.MIN_NORMAL,
        f64v = -0.0,
        c = "🦀".codePointAt(0),
    )

    fun bag(n: Int): Bag = Bag(
        name = "bag-$n-ünïcödé-🎒",
        tags = List(n) { "t$it" },
        shapes = listOf(Shape.Circle(1.5), Shape.Rect(2.0 to -3.0), Shape.Label("x".repeat(n)), Shape.Empty),
        color = if (n % 2 == 0) Color.BLUE else null,
        nested = when (n % 3) {
            0 -> null
            1 -> Some(null)
            else -> Some(n.toUInt())
        },
        pair = Triple(n.toUByte(), "", 0.25f),
        small = Small.A or Small.C,
        wide = Wide.ALL,
        bytes = ByteArray(n * 7) { it.toByte() },
    )

    /** Runs every check; throws on the first failure. */
    fun runSuite(t: Types, sink: Sink) {
        // Scalars and edge values.
        for (seed in 0 until 4) eq(t.echoScalars(scalars(seed)), scalars(seed), "echoScalars($seed)")
        val back = t.echoScalars(scalars(0).copy(f64v = Double.NaN, f32v = Float.POSITIVE_INFINITY))
        check(back.f64v.isNaN() && back.f32v == Float.POSITIVE_INFINITY) { "NaN/inf: $back" }

        // Strings and bytes, including empty and large.
        for (s in listOf("", "a", "héllo wörld", "🦀🦀🦀", "z".repeat(1 shl 16))) eq(t.echoString(s), s, "echoString")
        check(t.echoBytes(ByteArray(0)).isEmpty())
        val big = ByteArray(1_000_000) { (it * 31).toByte() }
        check(t.echoBytes(big).contentEquals(big)) { "echoBytes(1MB)" }

        // Records, variants, lists of records.
        for (n in 0 until 5) eq(t.echoBag(bag(n)), bag(n), "echoBag($n)")
        val bags = List(20) { bag(it) }
        eq(t.echoBags(bags), bags, "echoBags")
        eq(t.echoBags(emptyList()), emptyList(), "echoBags([])")
        for (s in listOf(Shape.Circle(Double.MAX_VALUE), Shape.Rect(0.0 to 1.0), Shape.Label(""), Shape.Empty)) eq(t.echoShape(s), s, "echoShape")

        // Options, nested options, flags.
        for ((a, b) in listOf<Pair<String?, Some<UInt?>?>>(null to null, "x" to Some(null), "" to Some(7u))) eq(t.echoOptions(a, b), a to b, "echoOptions")
        eq(t.echoFlags(Small.NONE, Wide.NONE), Small.NONE to Wide.NONE, "echoFlags(none)")
        eq(t.echoFlags(Small.ALL, Wide.ALL), Small.ALL to Wide.ALL, "echoFlags(all)")
        eq(t.echoFlags(Small.B, Wide.F31 or Wide.F0), Small.B to (Wide.F31 or Wide.F0), "echoFlags")

        // Results are values; traps are a different channel.
        eq(t.check(4u), 4uL, "check(4)")
        eq(throws<FailureException> { t.check(5u) }.payload, Failure.BadInput("5 is odd"), "check(5)")
        eq(throws<FailureException> { t.check(1001u) }.payload, Failure.TooBig(1001u), "check(1001)")
        t.checkUnit(false)
        eq(throws<WitError> { t.checkUnit(true) }.payload, null, "checkUnit(true)")
        t.noResult(1u)
        val boom = trap { t.boom("kaboom") }
        eq(boom.code, WnaTrap.PANIC, "boom code ($boom)")
        check(boom.message.contains("kaboom")) { "boom message: $boom" }
        // The server survives a trap.
        eq(t.sum(listOf(1L, 2L, 3L, -4L)), 2L, "sum")
        eq(t.sum(emptyList()), 0L, "sum([])")
        eq(t.concat(listOf("a", "", "ç")), "aç", "concat")

        // Resources: constructor, methods, borrow, own transfer, statics, drop.
        val before = t.counterTotal()
        val a = t.newCounter(10)
        eq(a.add(5), 15L, "add")
        val b = t.newCounter(100)
        a.merge(b)
        eq(a.get(), 115L, "merge")
        eq(t.counterTotal() - before, 10L + 5 + 100 + 100, "total")
        val many = t.makeCounters(3u)
        eq(many.map { it.get() }, listOf(0L, 1L, 2L), "makeCounters")
        eq(t.consume(b), 100L, "consume")
        eq(b.handle, 0u, "consume moves the handle")
        val stale = a.handle
        a.close()
        val st = trap { Counter.fromHandle(t, stale).get() }
        eq(st.code, WnaTrap.BAD_HANDLE, "stale handle ($st)")
        many.forEach { it.close() }

        // Callbacks: the server calls back into a listener we own.
        sink.newListener().use { l ->
            eq(t.emit(l, listOf("one", "two")), 2u, "emit")
            eq(t.emit(l, emptyList()), 2u, "emit([])")
            eq(l.count(), 2u, "count")
        }
    }
}
