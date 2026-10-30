// WNA runtime for Swift. Generated bindings build on it; app code sees
// `WnaTrap` and nothing else.

import CWna
import Foundation

/// A call that failed as a call (the other side panicked, a handle was
/// stale, the feature isn't loaded...). Domain errors are separate,
/// generated `<Function>Error` types.
public struct WnaTrap: Error, CustomStringConvertible, Sendable {
    public let code: Int32
    public let message: String

    public init(code: Int32, message: String) {
        self.code = code
        self.message = message
    }

    public static func invalid(_ m: String) -> WnaTrap { WnaTrap(code: WNA_TRAP_INVALID, message: m) }
    public static func badHandle(_ h: UInt32) -> WnaTrap { WnaTrap(code: WNA_TRAP_BAD_HANDLE, message: "stale or foreign handle \(h)") }

    public var description: String { "wna trap \(code): \(message)" }
}

/// Calls the slot at `offset` and turns a nonzero status into a `WnaTrap`.
@inline(__always)
public func wnaInvoke(_ iface: wna_iface, _ offset: Int, _ args: UnsafeRawPointer?, _ ret: UnsafeMutableRawPointer?, _ what: String) throws {
    let st = wna_call(iface, offset, args, ret, wna_libc_alloc())
    if st != WNA_OK {
        throw WnaTrap(code: st, message: "\(what): \(wnaTrapMessage(of: iface, offset))")
    }
}

/// The exporter's message for the last trap on this thread.
func wnaTrapMessage(of iface: wna_iface, _ offset: Int) -> String {
    guard let h = iface.table?.assumingMemoryBound(to: wna_table_header.self).pointee else { return "interface not bound" }
    if Int(h.size) < offset + 8 { return "the exporter's table predates this function" }
    var msg = wna_string()
    h.last_trap?(iface.inst, &msg)
    return wnaString(msg, owned: false)
}

// MARK: - Memory

/// Where lowered strings and lists live: a scratch arena for parameters
/// (freed after the call), or the caller's allocator for export results.
/// Lifting with `owned` frees result buffers after copying them.
public final class WnaCx {
    public let owned: Bool
    let alloc: UnsafePointer<wna_alloc>?
    var scratch: [UnsafeMutableRawPointer] = []
    var allocated: [(UnsafeMutableRawPointer, Int, Int)] = []
    public let exports: WnaExportBox?

    /// Import side: lowered params in scratch memory; lifted results are freed.
    public init() {
        owned = true
        alloc = nil
        exports = nil
    }

    /// Export side: params are views; results go through the caller's allocator.
    public init(exporting box: WnaExportBox, alloc: UnsafePointer<wna_alloc>?) {
        owned = false
        self.alloc = alloc
        exports = box
    }

    deinit {
        for p in scratch { free(p) }
    }

    public func bytes(_ size: Int, align: Int) throws -> UnsafeMutableRawPointer? {
        if size == 0 { return nil }
        if let a = alloc {
            guard let p = a.pointee.realloc(a.pointee.ctx, nil, 0, align, size) else { throw WnaTrap(code: WNA_TRAP_OOM, message: "out of memory") }
            allocated.append((p, size, align))
            return p
        }
        guard let p = malloc(size) else { throw WnaTrap(code: WNA_TRAP_OOM, message: "out of memory") }
        scratch.append(p)
        return p
    }

    /// Releases what a failed export lowering allocated through the caller.
    func unwind() {
        guard let a = alloc else { return }
        for (p, size, align) in allocated { _ = a.pointee.realloc(a.pointee.ctx, p, size, align, 0) }
        allocated.removeAll()
    }

    public func string(_ s: String) throws -> wna_string {
        var s = s
        return try s.withUTF8 { u in
            let p = try bytes(u.count, align: 1)
            if let p, let base = u.baseAddress { p.copyMemory(from: base, byteCount: u.count) }
            return wna_string(ptr: p?.assumingMemoryBound(to: UInt8.self), len: u.count)
        }
    }

    public func array<C>(_ n: Int, _ type: C.Type = C.self) throws -> UnsafeMutablePointer<C>? {
        try bytes(n * MemoryLayout<C>.stride, align: MemoryLayout<C>.alignment)?.bindMemory(to: C.self, capacity: n)
    }

    /// Frees a buffer from a lifted result (allocated by `wna_libc_alloc`).
    public func release(_ p: UnsafeMutableRawPointer?) {
        if owned, let p { free(p) }
    }
}

public func wnaString(_ s: wna_string, owned: Bool) -> String {
    guard let p = s.ptr, s.len > 0 else { return "" }
    let r = String(decoding: UnsafeBufferPointer(start: p, count: s.len), as: UTF8.self)
    if owned { free(p) }
    return r
}

public func wnaChar(_ c: UInt32) throws -> Unicode.Scalar {
    guard let s = Unicode.Scalar(c) else { throw WnaTrap.invalid("invalid char \(c)") }
    return s
}

/// A WIT `result` in a value position (fields, lists). Function results
/// are `throws` instead.
public enum WnaResult<T, E> {
    case ok(T)
    case err(E)
}

extension WnaResult: Equatable where T: Equatable, E: Equatable {}
extension WnaResult: Sendable where T: Sendable, E: Sendable {}

// MARK: - Exports

/// Resource handles a Swift exporter hands out: generation-checked slots.
public final class WnaHandles: @unchecked Sendable {
    private var slots: [(gen: UInt8, obj: AnyObject?)] = []
    private var free: [Int] = []
    private let lock = NSLock()

    public init() {}

    public func insert(_ o: AnyObject) -> UInt32 {
        lock.lock(); defer { lock.unlock() }
        let i: Int
        if let f = free.popLast() { i = f } else { slots.append((0, nil)); i = slots.count - 1 }
        let g = slots[i].gen == 255 ? 1 : slots[i].gen + 1
        slots[i] = (g, o)
        return UInt32(g) << 24 | UInt32(i)
    }

    public func get(_ h: UInt32) throws -> AnyObject {
        lock.lock(); defer { lock.unlock() }
        let (i, g) = (Int(h & 0xFF_FFFF), UInt8(h >> 24))
        guard i < slots.count, slots[i].gen == g, let o = slots[i].obj else { throw WnaTrap.badHandle(h) }
        return o
    }

    @discardableResult
    public func remove(_ h: UInt32) throws -> AnyObject {
        let o = try get(h)
        lock.lock(); defer { lock.unlock() }
        slots[Int(h & 0xFF_FFFF)].obj = nil
        free.append(Int(h & 0xFF_FFFF))
        return o
    }
}

/// An exported interface's instance: the implementation plus one handle
/// table per resource type it exports. Its address is the table's `inst`.
public final class WnaExportBox: @unchecked Sendable {
    public let impl: Any
    public let resources: [WnaHandles]

    public init(_ impl: Any, resources: Int) {
        self.impl = impl
        self.resources = (0..<resources).map { _ in WnaHandles() }
    }

    /// Keeps the box alive for the process (native instances never die).
    public func iface(table: UnsafeRawPointer) -> wna_iface {
        wna_iface(table: table, inst: Unmanaged.passRetained(self).toOpaque())
    }

    public static func from(_ inst: UnsafeMutableRawPointer?) -> WnaExportBox {
        Unmanaged<WnaExportBox>.fromOpaque(inst!).takeUnretainedValue()
    }
}

/// Runs an export body: any thrown error becomes a status and a message
/// for `last_trap`; a lowering failure releases what it allocated.
public func wnaExport(_ inst: UnsafeMutableRawPointer?, _ alloc: UnsafePointer<wna_alloc>?, _ body: (WnaExportBox, WnaCx) throws -> Void) -> Int32 {
    let box = WnaExportBox.from(inst)
    let cx = WnaCx(exporting: box, alloc: alloc)
    do {
        try body(box, cx)
        return WNA_OK
    } catch let t as WnaTrap {
        cx.unwind()
        wnaSetLastTrap(t.message)
        return t.code
    } catch {
        cx.unwind()
        wnaSetLastTrap(String(describing: error))
        return WNA_TRAP_PANIC
    }
}

private let trapKey: pthread_key_t = {
    var k = pthread_key_t()
    pthread_key_create(&k) { free($0) }
    return k
}()

public func wnaSetLastTrap(_ m: String) {
    free(pthread_getspecific(trapKey))
    let bytes = Array(m.utf8)
    let p = malloc(bytes.count + 8)!
    p.storeBytes(of: bytes.count, as: Int.self)
    (p + 8).copyMemory(from: bytes, byteCount: bytes.count)
    pthread_setspecific(trapKey, p)
}

/// The `last_trap` entry of every table Swift exports.
public let wnaLastTrap: @convention(c) (UnsafeMutableRawPointer?, UnsafeMutablePointer<wna_string>?) -> Void = { _, msg in
    guard let p = pthread_getspecific(trapKey) else {
        msg?.pointee = wna_string(ptr: nil, len: 0)
        return
    }
    msg?.pointee = wna_string(ptr: (p + 8).assumingMemoryBound(to: UInt8.self), len: p.load(as: Int.self))
}

public func wnaHeader(size: Int) -> wna_table_header {
    wna_table_header(size: UInt32(size), reserved: 0, last_trap: wnaLastTrap)
}
