# WNA — WIT-native platform

One interface language (WIT) and one binary contract for every boundary in
the app: Swift and Kotlin shells, a Rust (and C++) core, and features that are
either compiled in or downloaded later as signed, sandboxed Wasm components.
The same `.wit` produces every binding, and the same feature source ships as
both a built-in and a downloadable build.

This repository implements the proposal *WIT-Native Platform for a
Cross-Platform Monorepo* and the *WIT Native ABI system spec* it builds on.
The [deviations](#deviations-from-the-proposal) section below says where and
why it differs from them.

## Quick start

```sh
nix develop          # Rust 1.99.1 (+ wasm, iOS, Android targets), JDK 21, Kotlin, wasm-tools, just
just test            # macOS: Rust, C, C++ (ASan/UBSan), Swift, the app, and the iOS shell in the Simulator
```

Linux and CI use the same flake inside Docker:

```sh
docker build -t wna-dev .
docker run --rm -v "$PWD":/src -w /src wna-dev just test-linux
```

To generate a signing key, use `cargo run -p wna-pkg -- keygen -o keys/dev-signing.key`.
It prints the public key, which goes into `trusted-keys` in
`apps/wna.compose.toml`. The repo pins a development key; the private half is
`.gitignore`d.

## Layout

```
wit/                      interfaces, one directory per package
  core/ shell/ feature/   acme:core (storage), acme:shell (platform services), acme:feature (feature worlds)
  app/                    acme:app worlds: `core` (the Rust core) and `shell` (what Swift/Kotlin implement)
  host/                   wna:host/features: install, rollback, status of downloadable features
  test/                   wna:test: the conformance package (every type, resources, callbacks, traps)
tools/wna/
  abi/                    the model: WIT loading, layouts, table slot order (shared by everything)
  gen/                    wna-gen: C headers, Rust, Swift, C++ (and Kotlin) bindings
  rt/                     `wna` crate: Rust runtime + `wna::generate!` / `wna::compose!`
  macro/                  the proc macros
  host/                   feature host: Wasm bridge, signed packages, hot-swappable slots, composition runtime
  pkg/                    wna-pkg: componentize, keygen, build/sign/precompile, inspect
  swift/ cpp/ kotlin/     hand-written runtimes the generated code uses (Kotlin: + the generic JNI shim)
core/rust/                acme:core-rs: storage, persisted to the shell's data directory
features/smart-reminders/ a feature: one source, built natively (fallback) and as Wasm (package)
apps/
  wna.compose.toml        the composition: instances and who supplies each import (checked at build time)
  native/                 acme-app: the Rust half of the app (staticlib/cdylib) + the composition root
  rust-shell/             a desktop shell in Rust, with the end-to-end test
  ios/                    the iOS (and macOS) shell: SwiftPM package over an XCFramework
tests/conformance/        the cross-binding matrix (Rust, Wasm/Cranelift, Wasm/Pulley, Swift, C++, Kotlin)
abi/golden/               checked-in generated headers: `just golden` fails on ABI drift
```

## How it works

**The binary contract** (`tools/wna/gen/src/wna.h`). Each component exports a
single descriptor symbol. Each interface is reached through a function table
obtained from an instance, so an import is a parameter supplied at
composition, never a global symbol lookup. All table slots have the same C
shape:

```c
int32_t f(void *inst, const ARGS *args, RET *ret, const wna_alloc *alloc);
```

`ARGS` is a struct of the parameters (a method's `self` handle comes first),
and `RET` is the result's layout. Values cross by copy. Resources cross as
generation-checked `u32` handles into tables owned by their exporter. Every
buffer is freed by the allocator that allocated it: parameters are views, and
results are allocated through the caller's `alloc`. Boundaries never unwind:
panics and exceptions become status codes at the edge, while domain errors
travel as `result` values.

Tables are append-only within a major version. Slots are ordered by
`@since`, and a call to a slot beyond the exporter's table size returns
`TRAP_UNAVAILABLE`. That lets an importer built against `2.1` work with a
`2.0` exporter, with only the newer functions unavailable.

**Generators.** `wna-abi` computes layouts and slot order once. Each backend
renders them:

- **C headers:** every layout is checked by `_Static_assert`.
- **Rust:** `wna::generate!`, which has the same shapes as `wit-bindgen`, so
  one source builds natively or for Wasm.
- **Swift:** over the C headers as a Clang module.
- **C++20:** header-only.
- **Kotlin:** pure generated Kotlin over one fixed JNI library
  (`tools/wna/kotlin/jni/wna_jni.c`). Because every slot has the same C shape,
  nothing per interface needs JNI code. The same path serves Android and the
  desktop JVM.

**Composition.** `apps/wna.compose.toml` lists the instances and wires each
import. `wna::compose!` rejects a bad graph at compile time: an unwired
import, an incompatible version or a cycle. It emits a plan that
`wna_host::compose` executes at startup. Shells call one C function,
`wna_compose(shell_exports, out_imports)`; the generated Swift wraps it as
`ShellWorld.compose(...)`.

**Downloadable features.** A feature targets a published world, such as
`acme:feature/rules-engine`. That world's import list is its entire
capability set. Features are built for `wasm32-unknown-unknown` and
componentized without a WASI adapter, so the component imports exactly its
world and nothing more.

`wna-pkg` signs the component with Ed25519 and can attach precompiled
artifacts, for example `pulley64` for iOS. The manifest hashes cover those
artifacts.

At runtime the feature host verifies the signature, checks world and app
versions, and instantiates the component under memory and fuel limits. It then
atomically swaps the slot's target, so the importer keeps the same table it
bound at startup. A trap restarts the instance; repeated traps fall back to
the built-in implementation. `rollback` reinstates the previous package or the
built-in.

The bridge is generic. Because every slot has the same C shape, const-generic
trampolines turn any Rust object into a table. Values are converted between C
layouts and Wasmtime's dynamic `Val`s using the same layout model, so no code
is generated per world.

**Execution.** Android and desktop compile features with Cranelift. iOS
builds leave Cranelift out and run precompiled Pulley bytecode in the
interpreter: no JIT and no downloaded machine code. iOS also uses small
linear-memory reservations.

## What the tests prove

| Suite | What runs |
|---|---|
| `cargo test -p wna-test-rust-client` | The full suite (every v0 type, edge values, traps, stale handles, resources, callbacks) against a Rust server: natively, as a Wasm component under Cranelift, and under Pulley. All three use the same Rust source. |
| `just swift-test` | Swift client ↔ Rust server, Rust client ↔ Swift server, Swift ↔ Swift |
| `just cpp-test` | C++ client ↔ Rust server, Rust client ↔ C++ server, C++ ↔ C++, under ASan and UBSan (findings are fatal) |
| `just kotlin-test` | Kotlin client ↔ Rust server, Rust client (on a native thread) ↔ Kotlin server, Kotlin ↔ Kotlin, 8 concurrent threads, and version and closed-export traps, under `java -Xcheck:jni`. It also compiles bindings for every world in `wit/` |
| `cargo test -p acme-rust-shell` | The app end to end through `wna_compose`: the native fallback, hot-installing the signed Wasm build into the same slot with storage shared, rejecting a package with an untrusted signature, one that imports outside its world, or one built for a newer world, then rollback |
| `just ios-test` | The same flow from the iOS shell, on macOS and inside the iOS Simulator, where the feature runs as precompiled Pulley bytecode |
| `just golden`, `just c-headers` | Generated headers match `abi/golden`, and compile as C11 and C++20 with every layout assertion passing |

## Developer workflow: a new feature

1. **Check the world.** If the feature needs a new capability, add the import
   to `wit/feature/*.wit` with `@since(version = x.y.z)`. That change is a
   capability review.
2. **Write the feature in Rust.** Call `wna::generate!({ world: "acme:feature/rules-engine", path: "../../wit" })`
   and implement `exports::…::Guest`. Use the module paths and types exactly
   as with wit-bindgen.
3. **Wire the fallback.** Add a `[instances.<name>]` entry with `rust = "<crate>"`
   and add it as the slot's `fallback` in `apps/wna.compose.toml`. The
   build checks the wiring.
4. **Use it from a shell.** Call the generated API, for example
   `Rules.evaluate(...)` in Swift or `rules::evaluate(...)` in Rust. Nothing
   touches FFI.
5. **Publish.** Run `just component <crate> out/x.wasm`, then
   `wna-pkg build … --precompile pulley64 -o x.wnapkg`. The shell fetches the
   bytes and calls `Features.install(slot:package:)`.

## Deviations from the proposal

- **Build graph: Nix + Cargo + `just`, not Bazel.**
  - Phase 1 needed to run here, and there is no Android SDK on this machine.
  - Getting `rules_apple`, `rules_android` and `rules_rust` working under Nix
    on macOS is a project of its own.
  - The generator is a CLI plus proc macros, so `wit_package` and
    `wna_*_bindings` Bazel rules can wrap it as thin genrules later.
  - Cargo already gives the Rust graph its incremental rebuilds. `just`
    sequences Swift, C++ and Wasm.
- **One C shape for every slot**, `(inst, args*, ret*, alloc*)`, instead of the
  spec's flattened parameters with join and spill rules.
  - Flattening exists to fit Wasm core types and buys nothing natively.
  - Dropping it removes a whole section of rules from every backend.
  - It makes three things generic code instead of generated code: the Wasm
    bridge, the feature slots' forwarding tables, and the Kotlin binding,
    which needs one fixed JNI shim.
- **Wasm features have no WASI.** They target `wasm32-unknown-unknown` and are
  componentized without an adapter, so the sandbox is exactly the world.
- **Package verification happens in the Rust host, not in each shell.** There
  is one implementation; the shell only fetches the bytes.
- **The fallback is an import of the feature host.** A slot's built-in
  implementation is wired like any other import (`fallback = "<instance>"`),
  so it is checked at build time like everything else.
- **Swift domain errors:** a named payload type (`Storage.Error`) is thrown
  directly; other payloads are wrapped in a generated `<Function>Error`.
- **Swift tuples stay tuples.** Records and variants containing tuples
  therefore don't get synthesized `Equatable`.
- **Composition is an executable plan, not generated code.**
  `wna::compose!` validates the TOML and emits data; `wna_host::compose`
  runs it.

## Status against the rollout

| Phase | State |
|---|---|
| 0. Generator | Done: C header, Rust, C++, the function-table contract, the conformance world, golden headers, C++ under ASan/UBSan. Not done: sanitizers on the Rust glue (needs nightly), fuzzing the lifting code. |
| 1. Shells | iOS: the Swift binding and shell, composition, XCFramework, tests in the Simulator. Android: the Kotlin/JNI binding, tested on the desktop JVM through the same JNI path; the Android app module isn't built because there's no Android SDK here. |
| 2. Core on WNA | No legacy bridges exist in this greenfield repo; the core is WNA-only. |
| 3. Features | Done: Wasmtime bridge, slots with fallback, signing, install/rollback/status, Pulley on iOS, AOT artifacts. Not done: fetching (shell networking), feature A/B. |

Not implemented from the spec:

- `async`, `stream` and `future`.
- `lend` (zero-copy bytes).
- `[affinity]` main-thread checks.
- `reentrance = "trap"`.
- `serialize = true`.
- Wasm instance pools and per-thread instances; only `shared` is implemented.
- `wna-rt` plugin loading by name.
- Own handles nested inside parameter types. These are rejected or
  unsupported: top-level own and borrow parameters work, and nested owns in
  results work.
- A desktop JVM backend over FFM: the JNI backend serves both desktop and
  Android.
- The WIT package `wna:*` produces a Rust module named `wna`, which collides
  with the `wna` crate. Crates that import `wna:*` interfaces write
  `::wna::generate!`.
