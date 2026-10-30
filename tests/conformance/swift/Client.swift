// The conformance suite in Swift: a port of the Rust client's `run_suite`.

import CWna
import Foundation
import Wna

struct Failed: Error, CustomStringConvertible { let description: String }

func expect(_ c: Bool, _ what: @autoclosure () -> String, line: Int = #line) throws {
    if !c { throw Failed(description: "line \(line): \(what())") }
}

func same<T>(_ a: T, _ b: T) -> Bool { String(reflecting: a) == String(reflecting: b) }

func scalars(_ seed: Int64) -> Types.Scalars {
    Types.Scalars(b: seed % 2 == 0, u8v: .max, u16v: .max - UInt16(seed), u32v: .max, u64v: .max - UInt64(seed),
                  s8v: .min, s16v: .min, s32v: .min + Int32(seed), s64v: .min, f32v: .leastNormalMagnitude, f64v: -0.0, c: "🦀")
}

func bag(_ n: UInt32) -> Types.Bag {
    Types.Bag(
        name: "bag-\(n)-ünïcödé-🎒",
        tags: (0..<n).map { "t\($0)" },
        shapes: [.circle(1.5), .rect((2.0, -3.0)), .label(String(repeating: "x", count: Int(n))), .empty],
        color: n % 2 == 0 ? .blue : nil,
        nested: n % 3 == 0 ? nil : (n % 3 == 1 ? .some(nil) : .some(n)),
        pair: (UInt8(n), "", 0.25),
        small: [.a, .c],
        wide: Types.Wide(rawValue: .max),
        bytes: (0..<(n * 7)).map { UInt8(truncatingIfNeeded: $0) })
}

public func runSwiftSuite() throws {
    for seed in Int64(0)..<4 { try expect(try Types.echoScalars(s: scalars(seed)) == scalars(seed), "scalars \(seed)") }
    var nan = scalars(0); nan.f64v = .nan; nan.f32v = .infinity
    let back = try Types.echoScalars(s: nan)
    try expect(back.f64v.isNaN && back.f32v == .infinity, "nan/inf")

    for s in ["", "a", "héllo wörld", "🦀🦀🦀", String(repeating: "z", count: 1 << 16)] {
        try expect(try Types.echoString(s: s) == s, "string \(s.prefix(10))")
    }
    try expect(try Types.echoBytes(b: []) == [], "empty bytes")
    let big = (0..<1_000_000).map { UInt8(truncatingIfNeeded: $0 * 31) }
    try expect(try Types.echoBytes(b: big) == big, "1MB bytes")

    for n in UInt32(0)..<5 { try expect(same(try Types.echoBag(b: bag(n)), bag(n)), "bag \(n)") }
    let bags = (UInt32(0)..<20).map(bag)
    try expect(same(try Types.echoBags(b: bags), bags), "bags")
    try expect(try Types.echoBags(b: []).isEmpty, "no bags")
    let shapes: [Types.Shape] = [.circle(.greatestFiniteMagnitude), .rect((0, 1)), .label(""), .empty]
    for s in shapes {
        try expect(same(try Types.echoShape(s: s), s), "shape \(s)")
    }

    let optionCases: [(String?, UInt32??)] = [(nil, nil), ("x", .some(nil)), ("", .some(7))]
    for (a, b) in optionCases {
        let r = try Types.echoOptions(a: a, b: b)
        try expect(r.0 == a && r.1 == b, "options \(String(describing: a)) \(String(describing: b))")
    }
    let all = Types.Wide(rawValue: .max)
    let flagCases: [(Types.Small, Types.Wide)] = [([], []), ([.a, .b, .c], all), ([.b], [.f31, .f0])]
    for (a, b) in flagCases {
        let r = try Types.echoFlags(a: a, b: b)
        try expect(r.0 == a && r.1 == b, "flags")
    }

    try expect(try Types.check(n: 4) == 4, "check ok")
    do { _ = try Types.check(n: 5); try expect(false, "check 5 must fail") } catch let e as Types.Failure { try expect(e == .badInput("5 is odd"), "\(e)") }
    do { _ = try Types.check(n: 1001); try expect(false, "check 1001 must fail") } catch let e as Types.Failure { try expect(e == .tooBig(1001), "\(e)") }
    try Types.checkUnit(fail: false)
    do { try Types.checkUnit(fail: true); try expect(false, "check-unit must fail") } catch is Types.CheckUnitError {}
    try Types.noResult(n: 1)
    do { _ = try Types.boom(msg: "kaboom"); try expect(false, "boom must trap") } catch let t as WnaTrap {
        try expect(t.code == WNA_TRAP_PANIC && t.message.contains("kaboom"), "\(t)")
    }
    try expect(try Types.sum(xs: [1, 2, 3, -4]) == 2, "sum")
    try expect(try Types.sum(xs: []) == 0, "empty sum")
    try expect(try Types.concat(xs: ["a", "", "ç"]) == "aç", "concat")

    let before = try Types.Counter.total()
    var a: Types.Counter? = try Types.Counter(start: 10)
    try expect(try a!.add(n: 5) == 15, "add")
    let b = try Types.Counter(start: 100)
    try a!.merge(other: b)
    try expect(try a!.get() == 115, "merge")
    try expect(try Types.Counter.total() - before == 215, "total")
    let many = try Types.makeCounters(n: 3)
    try expect(try many.map { try $0.get() } == [0, 1, 2], "make-counters")
    try expect(try Types.consume(c: b) == 100, "consume")
    let stale = a!.handle
    a = nil
    do { _ = try Types.Counter(borrowing: stale).get(); try expect(false, "stale handle must trap") } catch let t as WnaTrap {
        try expect(t.code == WNA_TRAP_BAD_HANDLE, "\(t)")
    }

    let l = try Sink.Listener()
    try expect(try Types.emit(to: l, events: ["one", "two"]) == 2, "emit")
    try expect(try Types.emit(to: l, events: []) == 2, "emit none")
    try expect(try l.count() == 2, "listener count")
}
