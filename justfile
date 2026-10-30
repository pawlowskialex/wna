# WNA monorepo tasks. Run inside `nix develop` (or with direnv).

set shell := ["bash", "-euo", "pipefail", "-c"]

wasm_target := "wasm32-unknown-unknown"
target := env_var_or_default("CARGO_TARGET_DIR", "target")
pkg := target / "debug/wna-pkg"

default: test

# Every test on macOS: Rust, C, C++, Swift, the app, iOS (Simulator).
test: wasm-test-server features
    cargo test --workspace
    just c-headers golden cpp-test swift-test ios-test

# Every test that runs on Linux (CI image): no Swift/iOS.
test-linux: wasm-test-server features
    cargo test --workspace
    just c-headers golden cpp-test

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
    {{pkg}} componentize {{target}}/{{wasm_target}}/release/$(echo {{crate}} | tr - _).wasm -o {{out}}

wasm-test-server: (component "wna-test-rust-server" "out/wasm/wna-test-rust-server.wasm")

# Every feature as a component.
features: (component "smart-reminders" "out/wasm/smart-reminders.wasm")

swift_flags := "-swift-version 5 -O"

# Builds a Swift library module from sources (static).
_swift-module name out +srcs:
    swiftc {{swift_flags}} -parse-as-library -emit-library -static -module-name {{name}} -I out/swift/conformance/CWna/include -I {{out}} -emit-module -emit-module-path {{out}}/{{name}}.swiftmodule -o {{out}}/lib{{name}}.a {{srcs}}

# Swift ↔ Rust conformance on this machine (macOS or Linux).
swift-test:
    cargo build -q -p wna-test-fixture
    cargo run -q -p wna-gen -- swift --wit wit --world wna:test/server --out out/swift/conformance
    cargo run -q -p wna-gen -- swift --wit wit --world wna:test/client --out out/swift/conformance
    mkdir -p out/swift/build
    just _swift-module Wna out/swift/build out/swift/conformance/Wna/WnaRuntime.swift
    just _swift-module ConfServer out/swift/build out/swift/conformance/Server/ServerWorld.swift tests/conformance/swift/Server.swift
    just _swift-module ConfClient out/swift/build out/swift/conformance/Client/ClientWorld.swift tests/conformance/swift/Client.swift
    swiftc {{swift_flags}} -I out/swift/conformance/CWna/include -I tests/conformance/swift/include -I out/swift/build \
        tests/conformance/swift/main.swift -L out/swift/build -lWna -lConfServer -lConfClient \
        -L {{target}}/debug -lwna_test_fixture -o out/swift/build/conformance
    out/swift/build/conformance

ios_targets := "aarch64-apple-ios aarch64-apple-ios-sim"

# A signed package of the smart-reminders feature, precompiled for Pulley (iOS).
feature-package out: features _pkg-tool
    {{pkg}} build --component out/wasm/smart-reminders.wasm --wit wit --world acme:feature/rules-engine \
        --name smart-reminders --version 1.1.0 --min-app 1.0.0 --key keys/dev-signing.key --precompile pulley64 -o {{out}}

# Bindings + XCFramework for the iOS/macOS shell package (apps/ios).
ios-gen: (feature-package "apps/ios/Tests/AcmeShellTests/Fixtures/smart-reminders.wnapkg")
    rm -rf apps/ios/Generated && cargo run -q -p wna-gen -- swift --wit wit --world acme:app/shell --compose --out apps/ios/Generated
    for t in {{ios_targets}}; do cargo rustc -q -p acme-app --lib --crate-type staticlib --target $t --no-default-features --release; done
    cargo rustc -q -p acme-app --lib --crate-type staticlib --release
    rm -rf apps/ios/Native && xcodebuild -create-xcframework \
        -library {{target}}/release/libacme_app.a \
        -library {{target}}/aarch64-apple-ios/release/libacme_app.a \
        -library {{target}}/aarch64-apple-ios-sim/release/libacme_app.a \
        -output apps/ios/Native/AcmeNative.xcframework > /dev/null

# The iOS shell's tests on macOS (Cranelift) and in the iOS Simulator (Pulley).
ios-test sim="iPhone 17 Pro": ios-gen
    cd apps/ios && swift test
    cd apps/ios && xcodebuild test -scheme AcmeShell -destination "platform=iOS Simulator,name={{sim}}" -quiet

# System libraries a Rust staticlib needs when linked by a C/C++ toolchain.
rust_sys_libs := if os() == "linux" { "-lpthread -ldl -lm" } else { "" }

cxx := "clang++ -std=c++20 -g -O1 -Wall -Wextra -Werror -fsanitize=address,undefined -fno-omit-frame-pointer"

# C++ <-> Rust conformance under ASan and UBSan.
cpp-test:
    cargo build -q -p wna-test-fixture
    cargo run -q -p wna-gen -- cpp --wit wit --world wna:test/server --out out/cpp/server
    cargo run -q -p wna-gen -- cpp --wit wit --world wna:test/client --out out/cpp/client
    mkdir -p out/cpp/build
    {{cxx}} -c -Iout/cpp/server tests/conformance/cpp/server.cpp -o out/cpp/build/server.o
    {{cxx}} -c -Iout/cpp/client tests/conformance/cpp/client.cpp -o out/cpp/build/client.o
    {{cxx}} -Iout/cpp/client tests/conformance/cpp/main.cpp out/cpp/build/server.o out/cpp/build/client.o \
        {{target}}/debug/libwna_test_fixture.a {{rust_sys_libs}} -o out/cpp/build/conformance
    ASAN_OPTIONS=detect_leaks=0 out/cpp/build/conformance
