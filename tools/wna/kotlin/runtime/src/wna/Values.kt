package wna

/** The `some` case of an `option<T>` whose `T` is itself nullable: `option<option<u32>>` is `Some<UInt?>?`. */
public data class Some<out T>(public val value: T)

/** A `result<T, E>` that is not a function's return value (those throw [WitError] instead). */
public sealed interface WitResult<out T, out E> {
    public data class Ok<out T>(public val value: T) : WitResult<T, Nothing>
    public data class Err<out E>(public val value: E) : WitResult<Nothing, E>
}

/** Tuples of arity 2 and 3 are `Pair` and `Triple`. */
public data class Tuple1<out A>(public val first: A)
public data class Tuple4<out A, out B, out C, out D>(val first: A, val second: B, val third: C, val fourth: D)
public data class Tuple5<out A, out B, out C, out D, out E>(val first: A, val second: B, val third: C, val fourth: D, val fifth: E)
public data class Tuple6<out A, out B, out C, out D, out E, out F>(val first: A, val second: B, val third: C, val fourth: D, val fifth: E, val sixth: F)
public data class Tuple7<out A, out B, out C, out D, out E, out F, out G>(val first: A, val second: B, val third: C, val fourth: D, val fifth: E, val sixth: F, val seventh: G)
public data class Tuple8<out A, out B, out C, out D, out E, out F, out G, out H>(val first: A, val second: B, val third: C, val fourth: D, val fifth: E, val sixth: F, val seventh: G, val eighth: H)
