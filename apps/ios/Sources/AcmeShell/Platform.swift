// The iOS platform services this shell exports to the app.

import AcmeBindings
import Foundation
import Security
import UserNotifications
import os

/// Secrets in the Keychain (`acme:shell/secrets`).
public struct KeychainSecrets: Secrets.Exports {
    let service: String
    public init(service: String = Bundle.main.bundleIdentifier ?? "acme") { self.service = service }

    private func query(_ name: String) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: name]
    }

    public func get(name: String) throws -> String? {
        var q = query(name)
        q[kSecReturnData as String] = true
        var out: CFTypeRef?
        guard SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess, let d = out as? Data else { return nil }
        return String(decoding: d, as: UTF8.self)
    }

    public func set(name: String, value: String) throws {
        SecItemDelete(query(name) as CFDictionary)
        var q = query(name)
        q[kSecValueData as String] = Data(value.utf8)
        q[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
        let st = SecItemAdd(q as CFDictionary, nil)
        if st != errSecSuccess { throw NSError(domain: NSOSStatusErrorDomain, code: Int(st)) }
    }
}

/// Local notifications (`acme:shell/notify`).
public struct UserNotifications: Notify.Exports {
    public init() {}

    public func schedule(n: Notify.Notification) throws {
        let content = UNMutableNotificationContent()
        content.title = n.title
        content.body = n.body
        let delay = max(1, Date(timeIntervalSince1970: Double(n.atMs) / 1000).timeIntervalSinceNow)
        let request = UNNotificationRequest(identifier: n.id, content: content, trigger: UNTimeIntervalNotificationTrigger(timeInterval: delay, repeats: false))
        UNUserNotificationCenter.current().add(request) { error in
            if let error { Log.shell.error("scheduling \(n.id, privacy: .public): \(error.localizedDescription, privacy: .public)") }
        }
    }

    public func cancel(id: String) throws {
        UNUserNotificationCenter.current().removePendingNotificationRequests(withIdentifiers: [id])
    }
}

/// App configuration (`acme:shell/config`): where the core keeps its data.
public struct AppConfig: Config.Exports {
    let values: [String: String]

    public init(dataDirectory: URL? = nil) {
        let dir = dataDirectory ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("acme")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        values = ["data-dir": dir.path]
    }

    public func get(key: String) throws -> String? { values[key] }
}

enum Log {
    static let shell = Logger(subsystem: "acme", category: "shell")
}

/// Unified logging (`wasi:logging/logging`).
public struct OSLogging: Logging.Exports {
    public init() {}

    public func log(level: Logging.Level, context: String, message: String) throws {
        let l = Logger(subsystem: "acme", category: context)
        switch level {
        case .trace, .debug: l.debug("\(message, privacy: .public)")
        case .info: l.info("\(message, privacy: .public)")
        case .warn: l.warning("\(message, privacy: .public)")
        case .error: l.error("\(message, privacy: .public)")
        case .critical: l.critical("\(message, privacy: .public)")
        }
    }
}
