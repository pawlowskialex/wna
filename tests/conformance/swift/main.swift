// Swift ↔ Rust conformance, both directions, plus Swift ↔ Swift.

import CFixture
import CWna
import ConfClient
import ConfServer
import Foundation
import Wna

func order(_ names: [String], _ tables: [String: wna_iface]) -> [wna_iface] {
    names.map { n in tables.first { n.hasPrefix($0.key) }!.value }
}

func run(_ name: String, _ body: () throws -> Void) -> Bool {
    do {
        try body()
        print("ok    \(name)")
        return true
    } catch {
        print("FAIL  \(name): \(error)")
        return false
    }
}

var sink = wna_iface()
wna_fixture_rust_sink(&sink)
var rustTypes = wna_iface()
wna_fixture_rust_server(sink, &rustTypes)

ServerWorld.bind(order(ServerWorld.imports, ["wna:test/sink": sink]))
let swiftTypes = ConfServer.Types.table(SwiftServer())

var ok = true
ok = run("swift client -> rust server") {
    ClientWorld.bind(order(ClientWorld.imports, ["wna:test/types": rustTypes, "wna:test/sink": sink]))
    try runSwiftSuite()
} && ok
ok = run("rust client -> swift server") {
    var msg = wna_string()
    if wna_fixture_run_rust_client(swiftTypes, sink, &msg) != 0 {
        throw WnaTrap(code: 1, message: String(decoding: UnsafeBufferPointer(start: msg.ptr, count: msg.len), as: UTF8.self))
    }
} && ok
ok = run("swift client -> swift server") {
    ClientWorld.bind(order(ClientWorld.imports, ["wna:test/types": swiftTypes, "wna:test/sink": sink]))
    try runSwiftSuite()
} && ok
exit(ok ? 0 : 1)
