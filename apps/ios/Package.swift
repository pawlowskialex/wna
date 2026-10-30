// swift-tools-version: 6.0
// The iOS (and macOS) shell. `just ios-gen` produces Generated/ (bindings)
// and Native/AcmeNative.xcframework (the Rust half: core, features, host).

import PackageDescription

let swift5: [SwiftSetting] = [.swiftLanguageMode(.v5)]

let package = Package(
    name: "AcmeShell",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [.library(name: "AcmeShell", targets: ["AcmeShell"])],
    targets: [
        .binaryTarget(name: "AcmeNative", path: "Native/AcmeNative.xcframework"),
        .target(name: "CWna", path: "Generated/CWna"),
        .target(name: "Wna", dependencies: ["CWna"], path: "Generated/Wna", swiftSettings: swift5),
        .target(name: "AcmeBindings", dependencies: ["CWna", "Wna"], path: "Generated/Shell", swiftSettings: swift5),
        .target(name: "AcmeShell", dependencies: ["AcmeBindings", "Wna", "AcmeNative"], swiftSettings: swift5),
        .testTarget(name: "AcmeShellTests", dependencies: ["AcmeShell"], resources: [.copy("Fixtures")], swiftSettings: swift5),
    ]
)
