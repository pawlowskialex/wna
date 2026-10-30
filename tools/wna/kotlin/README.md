# WNA for Kotlin (JVM and Android)

Every WNA table slot has the same C shape,
`int32_t f(void *inst, const void *args, void *ret, const wna_alloc *alloc)`,
so Kotlin needs no per-interface JNI or C code:

- `jni/wna_jni.c`: one fixed JNI library (`libwna_jni`). It calls slots
  (`WnaNative.call`, with the table-size check of `wna_call` and a malloc
  allocator for results), exposes native memory as direct `ByteBuffer`s, and
  provides 256 generic trampolines for exports. A trampoline attaches the
  calling thread to the JVM if needed (detached again at thread exit) and calls
  `WnaNative.dispatch(objId, slot, args, ret, alloc)`.
- `runtime/src/wna/*.kt`: `Iface`, `WnaTrap`, `WitError`, the per-call
  scratch `Arena`, `Lift`/`Lower`, `HandleTable`, and the `Export` registry.
- `wna-gen kotlin --wit wit --world <w>... [--package <prefix>] --out <dir>`:
  pure Kotlin that reads and writes values at the model's offsets.

## Generated API

For an interface `ns:pkg/name`, package `ns.pkg.name`:

| WIT | Kotlin |
|-----|--------|
| `u8`..`u64`, `s8`..`s64`, `f32`, `f64`, `bool` | `UByte`..`ULong`, `Byte`..`Long`, `Float`, `Double`, `Boolean` |
| `char` | `Int` (a Unicode scalar value, validated) |
| `string` | `String` (UTF-8 on the wire, strictly validated) |
| `list<u8>` / `list<T>` | `ByteArray` / `List<T>` |
| `option<T>` | `T?`; `wna.Some<T>?` when `T` is itself an option |
| `result<T, E>` as a return | returns `T`, throws `<E>Exception` (named `E`) or `wna.WitError` |
| `result<T, E>` elsewhere | `wna.WitResult<T, E>` |
| `tuple<..>` | `Pair`, `Triple`, `wna.Tuple1`/`Tuple4..8` |
| record / variant / enum / flags | data class / sealed interface / enum class / value class |

- Imported interface: `class Name(iface: wna.Iface, deps...)`. Constructors are
  `newRes(...)`, statics `resName(...)`; a resource is an `AutoCloseable`
  class holding its handle (`close()` drops it; `fromHandle`/`borrow` wrap raw
  handles). A failed call throws `wna.WnaTrap(code, message)`.
- Exported interface (`exports` subpackage): implement `interface Name` and
  `interface Res` per resource, then pass `NameExport(impl, deps...).iface` to
  native code. Exported resources live in a generation-checked
  `wna.HandleTable`; a stale handle is `BAD_HANDLE`. Any throwable escaping an
  implementation becomes a status (`WnaTrap` keeps its code, anything else is
  `PANIC`) with its message for `last_trap`; nothing unwinds into C.
- `deps` are the bindings of other interfaces whose handles this one lifts
  (e.g. a server's `TypesExport` takes the imported `Sink` to wrap listeners).

## Testing

`just kotlin-test` builds the shim with clang, generates bindings, compiles
everything with `kotlinc` and runs `tests/conformance/kotlin` on the desktop
JVM: the Kotlin port of the conformance suite against the Rust server and the
Kotlin server, the Rust client suite (on a native thread) against the Kotlin
server, plus threading and unavailable-slot checks.

Android cannot be built here (no Android SDK), but it uses the same code path:
the shim is plain JNI (`JNI_OnLoad`, `AttachCurrentThreadAsDaemon`, direct
buffers; the `JNIEnv**` signature difference is handled), the runtime and
bindings are plain Kotlin with no JVM-only APIs beyond `java.nio` and
`java.util.concurrent`. On Android, build `wna_jni.c` with the NDK into
`libwna_jni.so` next to the app's native components.

## Limitations

- Imported resources are freed by `close()`; there is no `Cleaner` fallback.
- `List<ByteArray>` inside other containers compares by reference (only
  direct `ByteArray` fields and payloads get content equality).
- Lowering a `borrow<r>` of a resource the Kotlin side itself exports is not
  supported yet; the generator reports it.
- Each `Arena` keeps one 16 KiB chunk per thread (pooled) for reuse.
