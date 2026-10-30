# WNA monorepo tasks. Run inside `nix develop` (or with direnv).

set shell := ["bash", "-euo", "pipefail", "-c"]

wasm_target := "wasm32-unknown-unknown"
pkg := "target/debug/wna-pkg"

default: test

# Every test: Rust units, the cross-binding conformance matrix, the app.
test: wasm-test-server features
    cargo test --workspace
    just c-headers

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
