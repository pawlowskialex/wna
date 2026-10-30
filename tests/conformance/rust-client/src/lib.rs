//! The conformance driver: calls `wna:test/types` through whatever server it
//! is bound to and checks every behaviour. Run against each binding's server.

::wna::generate!({ world: "wna:test/client", path: "../../../wit", name: "wna-test-rust-client", additional_derives: [PartialEq] });

use ::wna::abi::*;
use wna_test::sink::Listener;
use wna_test::types::*;

/// Path alias: the `wna:test` package's modules (the crate root also has `wna`).
mod wna_test {
    pub use super::wna::test::*;
}

/// Binds this driver to a server's `types` and a sink provider's `sink`.
pub fn bind(types: Iface, sink: Iface) {
    let imports = ::wna::compose::order_imports(__wna_world::IMPORTS, &[("wna:test/types", types), ("wna:test/sink", sink)]).unwrap();
    __wna_world::instantiate(&imports, 0);
}

fn scalars(seed: i64) -> Scalars {
    Scalars {
        b: seed % 2 == 0,
        u8v: u8::MAX,
        u16v: u16::MAX - seed as u16,
        u32v: u32::MAX,
        u64v: u64::MAX - seed as u64,
        s8v: i8::MIN,
        s16v: i16::MIN,
        s32v: i32::MIN + seed as i32,
        s64v: i64::MIN,
        f32v: f32::MIN_POSITIVE,
        f64v: -0.0,
        c: '🦀',
    }
}

fn bag(n: u32) -> Bag {
    Bag {
        name: format!("bag-{n}-ünïcödé-🎒"),
        tags: (0..n).map(|i| format!("t{i}")).collect(),
        shapes: vec![Shape::Circle(1.5), Shape::Rect((2.0, -3.0)), Shape::Label("x".repeat(n as usize)), Shape::Empty],
        color: if n % 2 == 0 { Some(Color::Blue) } else { None },
        nested: match n % 3 {
            0 => None,
            1 => Some(None),
            _ => Some(Some(n)),
        },
        pair: (n as u8, String::new(), 0.25),
        small: Small::A | Small::C,
        wide: Wide::all(),
        bytes: (0..n * 7).map(|i| i as u8).collect(),
    }
}

/// Runs every check; panics with a description on the first failure.
/// `isolated`: the server runs in a Wasm sandbox, where a guest panic aborts
/// without a message and the trapped instance is replaced by a fresh one.
pub fn run_suite(isolated: bool) {
    // Scalars and edge values.
    for seed in 0..4 {
        assert_eq!(echo_scalars(scalars(seed)), scalars(seed));
    }
    let nan = Scalars { f64v: f64::NAN, f32v: f32::INFINITY, ..scalars(0) };
    let back = echo_scalars(nan);
    assert!(back.f64v.is_nan() && back.f32v == f32::INFINITY);

    // Strings and bytes, including empty and large.
    for s in ["", "a", "héllo wörld", "🦀🦀🦀", &"z".repeat(1 << 16)] {
        assert_eq!(echo_string(s), s);
    }
    assert_eq!(echo_bytes(&[]), Vec::<u8>::new());
    let big: Vec<u8> = (0..1_000_000u32).map(|i| (i * 31) as u8).collect();
    assert_eq!(echo_bytes(&big), big);

    // Records, variants, lists of records.
    for n in 0..5 {
        assert_eq!(echo_bag(&bag(n)), bag(n));
    }
    let bags: Vec<Bag> = (0..20).map(bag).collect();
    assert_eq!(echo_bags(&bags), bags);
    assert_eq!(echo_bags(&[]), vec![]);
    for s in [Shape::Circle(f64::MAX), Shape::Rect((0.0, 1.0)), Shape::Label(String::new()), Shape::Empty] {
        assert_eq!(echo_shape(&s), s);
    }

    // Options, nested options, flags.
    for (a, b) in [(None, None), (Some("x"), Some(None)), (Some(""), Some(Some(7)))] {
        assert_eq!(echo_options(a, b), (a.map(str::to_string), b));
    }
    assert_eq!(echo_flags(Small::empty(), Wide::empty()), (Small::empty(), Wide::empty()));
    assert_eq!(echo_flags(Small::all(), Wide::all()), (Small::all(), Wide::all()));
    assert_eq!(echo_flags(Small::B, Wide::F31 | Wide::F0), (Small::B, Wide::F31 | Wide::F0));

    // Results are values; traps are a different channel.
    assert_eq!(check(4), Ok(4));
    assert_eq!(check(5), Err(Failure::BadInput("5 is odd".into())));
    assert_eq!(check(1001), Err(Failure::TooBig(1001)));
    assert_eq!(check_unit(false), Ok(()));
    assert_eq!(check_unit(true), Err(()));
    no_result(1);
    let t = ::wna::catch(|| boom("kaboom")).unwrap_err();
    assert_eq!(t.code, TRAP_PANIC, "{t}");
    if isolated {
        assert!(t.message.contains("trapped"), "{t}");
    } else {
        assert!(t.message.contains("kaboom"), "{t}");
    }
    // The server survives a trap.
    assert_eq!(sum(&[1, 2, 3, -4]), 2);
    assert_eq!(sum(&[]), 0);
    assert_eq!(concat(&["a".into(), "".into(), "ç".into()]), "aç");

    // Resources: constructor, methods, borrow, own transfer, statics, drop.
    let before = Counter::total();
    let a = Counter::new(10);
    assert_eq!(a.add(5), 15);
    let b = Counter::new(100);
    a.merge(&b);
    assert_eq!(a.get(), 115);
    assert_eq!(Counter::total() - before, 10 + 5 + 100 + 100);
    let many = make_counters(3);
    assert_eq!(many.iter().map(|c| c.get()).collect::<Vec<_>>(), vec![0, 1, 2]);
    assert_eq!(consume(b), 100);
    let stale = a.handle();
    drop(a);
    let t = ::wna::catch(|| unsafe { Counter::from_handle(stale) }.get()).unwrap_err();
    assert_eq!(t.code, TRAP_BAD_HANDLE, "{t}");
    drop(many);

    // Callbacks: the server calls back into a listener we own.
    let l = Listener::new();
    assert_eq!(emit(&l, &["one".into(), "two".into()]), 2);
    assert_eq!(emit(&l, &[]), 2);
    assert_eq!(l.count(), 2);
}
