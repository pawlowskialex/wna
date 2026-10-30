package wna

/**
 * A call that failed as a call (the exporter panicked, a handle was stale, a
 * slot is unavailable...). Distinct from a WIT `result` error, which is a
 * value (see [WitError]).
 */
public class WnaTrap(public val code: Int, override val message: String) : RuntimeException(message) {
    override fun toString(): String = "wna trap (${name(code)}): $message"

    public companion object {
        public const val OK: Int = 0
        public const val PANIC: Int = 1
        public const val BAD_HANDLE: Int = 2
        public const val OOM: Int = 3
        public const val INVALID: Int = 4
        public const val REENTRANCE: Int = 5
        public const val WRONG_THREAD: Int = 6
        public const val UNAVAILABLE: Int = 7
        public const val CANCELLED: Int = 8

        public fun invalid(what: String): WnaTrap = WnaTrap(INVALID, what)

        public fun badHandle(h: Int): WnaTrap = WnaTrap(BAD_HANDLE, "stale or foreign handle 0x" + Integer.toHexString(h))

        public fun name(code: Int): String = when (code) {
            OK -> "ok"
            PANIC -> "panic"
            BAD_HANDLE -> "bad handle"
            OOM -> "out of memory"
            INVALID -> "invalid value"
            REENTRANCE -> "reentrance"
            WRONG_THREAD -> "wrong thread"
            UNAVAILABLE -> "unavailable"
            CANCELLED -> "cancelled"
            else -> "unknown trap"
        }
    }
}

/**
 * The `err` case of a function returning `result<T, E>`: imports throw it,
 * exports throw it to return `err`. Named error types get a generated
 * subclass (`<E>Exception`); other error types use this class directly
 * (`payload` is `null` for `result<T>` / `result`).
 */
public open class WitError(payload: Any?) : RuntimeException("error result: $payload") {
    public open val payload: Any? = payload
}
