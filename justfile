# WNA monorepo tasks. Run inside `nix develop` (or with direnv).

set shell := ["bash", "-euo", "pipefail", "-c"]

wasm_target := "wasm32-unknown-unknown"
pkg := "target/debug/wna-pkg"

default: test

# Every test: Rust units, the cross-binding conformance matrix, the app.
test: wasm-test-server features
    cargo test --workspace
    just c-headers
    just kotlin-test

# Generated C headers compile as C11 and C++20 (their _Static_asserts check every layout).
c-headers:
    cargo run -q -p wna-gen -- c --wit wit --out out/c
    for h in out/c/*.h; do echo "#include \"$(basename $h)\""; done > out/c/all.c
    clang -std=c11 -Wall -Wextra -Werror -fsyntax-only -Iout/c out/c/all.c
    clang++ -std=c++20 -Wall -Wextra -Werror -fsyntax-only -Iout/c -x c++ out/c/all.c

# Golden ABI: fails when a generated header changes. Bless with `just golden-bless`.
golden:
    cargo run -q -p wna-gen -- c --wit wit --out out/golden
    diff -ru abi/golden out/golden/ || (echo "ABI changed: bump the package version, then 'just golden-bless'"; exit 1)

golden-bless:
    rm -rf abi/golden && cargo run -q -p wna-gen -- c --wit wit --out abi/golden

_pkg-tool:
    cargo build -q -p wna-pkg

# Builds a crate as a Wasm component: core module + componentize (no WASI).
component crate out: _pkg-tool
    cargo build -q -p {{crate}} --target {{wasm_target}} --release
    mkdir -p $(dirname {{out}})
    {{pkg}} componentize target/{{wasm_target}}/release/$(echo {{crate}} | tr - _).wasm -o {{out}}

wasm-test-server: (component "wna-test-rust-server" "out/wasm/wna-test-rust-server.wasm")

# Every feature as a component.
features: (component "smart-reminders" "out/wasm/smart-reminders.wasm")

kotlin_out := "out/kotlin"
dylib := if os() == "macos" { "dylib" } else { "so" }
jni_os := if os() == "macos" { "darwin" } else { "linux" }

# The generic JNI shim every Kotlin binding loads (`libwna_jni`).
kotlin-jni:
    mkdir -p {{kotlin_out}}/lib
    clang -std=c11 -shared -fPIC -O2 -Wall -Wextra -Werror -I"$JAVA_HOME/include" -I"$JAVA_HOME/include/{{jni_os}}" -Itools/wna/gen/src tools/wna/kotlin/jni/wna_jni.c -o {{kotlin_out}}/lib/libwna_jni.{{dylib}}

# Kotlin conformance: Kotlin client ↔ Rust server, Rust client ↔ Kotlin server
# (and Kotlin ↔ Kotlin), all through the JNI shim on the desktop JVM.
kotlin-test: kotlin-jni
    cargo build -q -p wna-test-kotlin-fixture
    cp target/debug/libwna_test_kotlin_fixture.{{dylib}} {{kotlin_out}}/lib/
    rm -rf {{kotlin_out}}/gen
    # The test worlds, plus every other world in the tree (compiled, not run).
    cargo run -q -p wna-gen -- kotlin --wit wit --out {{kotlin_out}}/gen \
        --world wna:test/client --world wna:test/server --world wna:test/sink-provider \
        --world acme:app/core --world acme:app/shell --world acme:feature/rules-engine --world wna:host/control
    kotlinc -nowarn -include-runtime -d {{kotlin_out}}/conformance.jar tools/wna/kotlin/runtime/src {{kotlin_out}}/gen tests/conformance/kotlin/src
    java -Xcheck:jni -Djava.library.path={{kotlin_out}}/lib -cp {{kotlin_out}}/conformance.jar wnatest.MainKt
