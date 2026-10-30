// The app as the iOS shell sees it: compose, the built-in feature, then the
// same feature hot-installed as a signed package. On iOS the package runs as
// precompiled Pulley bytecode in the interpreter: no JIT, no machine code.

import AcmeShell
import Foundation
import XCTest

final class MemorySecrets: Secrets.Exports, @unchecked Sendable {
    private var values: [String: String] = [:]
    private let lock = NSLock()
    func get(name: String) throws -> String? { lock.lock(); defer { lock.unlock() }; return values[name] }
    func set(name: String, value: String) throws { lock.lock(); values[name] = value; lock.unlock() }
}

final class RecordingNotify: Notify.Exports, @unchecked Sendable {
    private(set) var scheduled: [Notify.Notification] = []
    private let lock = NSLock()
    func schedule(n: Notify.Notification) throws { lock.lock(); scheduled.append(n); lock.unlock() }
    func cancel(id: String) throws { lock.lock(); scheduled.removeAll { $0.id == id }; lock.unlock() }
}

final class AcmeShellTests: XCTestCase {
    func testComposeFallbackThenHotSwap() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let notify = RecordingNotify()
        try AcmeApp.start(secrets: MemorySecrets(), notify: notify, config: AppConfig(dataDirectory: dir), logging: OSLogging())

        let now: UInt64 = 1_000_000_000
        try Storage.put(key: "reminder/dentist", value: Array("\(now + 3_600_000)|Dentist".utf8))
        XCTAssertEqual(try Rules.describe(), "smart-reminders 1.0.0 (native)")
        let actions = try Rules.evaluate(facts: [Rules.Fact(key: "now", value: "\(now)")])
        XCTAssertEqual(actions.map(\.target), ["dentist"])
        XCTAssertEqual(notify.scheduled.first?.atMs, now + 3_600_000 - 15 * 60_000)
        XCTAssertThrowsError(try Storage.get(key: "")) { e in
            guard case .invalidKey = e as? Storage.Error else { return XCTFail("\(e)") }
        }

        let url = try XCTUnwrap(Bundle.module.url(forResource: "smart-reminders", withExtension: "wnapkg", subdirectory: "Fixtures"))
        let version = try AcmeApp.install([UInt8](try Data(contentsOf: url)), slot: "rules")
        XCTAssertEqual(version, "1.1.0")
        XCTAssertEqual(try Rules.describe(), "smart-reminders 1.0.0 (wasm)")
        let status = try Features.status()
        XCTAssertEqual(status.first?.state, .active)

        try Storage.put(key: "reminder/standup", value: Array("\(now + 7_200_000)|Standup".utf8))
        let more = try Rules.evaluate(facts: [Rules.Fact(key: "now", value: "\(now)")])
        XCTAssertEqual(more.map(\.target), ["standup"], "the Wasm build sees what the native build stored")

        XCTAssertThrowsError(try AcmeApp.install([1, 2, 3], slot: "rules"))
        try Features.rollback(slot: "rules")
        XCTAssertEqual(try Rules.describe(), "smart-reminders 1.0.0 (native)")
    }
}
