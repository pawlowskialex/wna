// `wna:test/types` in Swift: the same behaviour as the Rust reference server.

import Foundation
import Wna

final class SwiftCounter: Types.CounterResource, @unchecked Sendable {
    private var value: Int64
    private let lock = NSLock()
    init(_ v: Int64) { value = v; SwiftServer.total.add(v) }
    func add(n: Int64) throws -> Int64 {
        SwiftServer.total.add(n)
        lock.lock(); defer { lock.unlock() }
        value += n
        return value
    }
    func get() throws -> Int64 { lock.lock(); defer { lock.unlock() }; return value }
    func merge(other: any Types.CounterResource) throws { _ = try add(n: other.get()) }
}

final class Total: @unchecked Sendable {
    private var v: Int64 = 0
    private let lock = NSLock()
    func add(_ n: Int64) { lock.lock(); v += n; lock.unlock() }
    var value: Int64 { lock.lock(); defer { lock.unlock() }; return v }
}

struct Boom: Error, CustomStringConvertible { let description: String }

public struct SwiftServer: Types.Exports {
    static let total = Total()
    public init() {}

    public func echoScalars(s: Types.Scalars) throws -> Types.Scalars { s }
    public func echoString(s: String) throws -> String { s }
    public func echoBytes(b: [UInt8]) throws -> [UInt8] { b }
    public func echoBag(b: Types.Bag) throws -> Types.Bag { b }
    public func echoBags(b: [Types.Bag]) throws -> [Types.Bag] { b }
    public func echoShape(s: Types.Shape) throws -> Types.Shape { s }
    public func echoOptions(a: String?, b: UInt32??) throws -> (String?, UInt32??) { (a, b) }
    public func echoFlags(a: Types.Small, b: Types.Wide) throws -> (Types.Small, Types.Wide) { (a, b) }
    public func check(n: UInt64) throws -> UInt64 {
        if n % 2 == 0 { return n }
        if n > 1000 { throw Types.Failure.tooBig(n) }
        throw Types.Failure.badInput("\(n) is odd")
    }
    public func checkUnit(fail: Bool) throws { if fail { throw Types.CheckUnitError() } }
    public func noResult(n: UInt32) throws {}
    public func boom(msg: String) throws -> UInt32 { throw Boom(description: msg) }
    public func sum(xs: [Int64]) throws -> Int64 { xs.reduce(0, +) }
    public func concat(xs: [String]) throws -> String { xs.joined() }
    public func counterNew(start: Int64) throws -> any Types.CounterResource { SwiftCounter(start) }
    public func counterTotal() throws -> Int64 { SwiftServer.total.value }
    public func makeCounters(n: UInt32) throws -> [any Types.CounterResource] { (0..<n).map { SwiftCounter(Int64($0)) } }
    public func consume(c: any Types.CounterResource) throws -> Int64 { try c.get() }
    public func emit(to: Sink.Listener, events: [String]) throws -> UInt32 {
        for e in events { try to.onEvent(e: e) }
        return try to.count()
    }
}
