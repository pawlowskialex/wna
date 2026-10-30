//! `wna:test/types` in Rust. The behaviour every binding's server must match.

use std::sync::atomic::{AtomicI64, Ordering};

::wna::generate!({ world: "wna:test/server", path: "../../../wit", name: "wna-test-rust-server", additional_derives: [PartialEq] });

use exports::wna::test::types::*;

pub struct Server;

static TOTAL: AtomicI64 = AtomicI64::new(0);

pub struct MyCounter(AtomicI64);

impl Guest for Server {
    type Counter = MyCounter;

    fn echo_scalars(s: Scalars) -> Scalars {
        s
    }
    fn echo_string(s: String) -> String {
        s
    }
    fn echo_bytes(b: Vec<u8>) -> Vec<u8> {
        b
    }
    fn echo_bag(b: Bag) -> Bag {
        b
    }
    fn echo_bags(b: Vec<Bag>) -> Vec<Bag> {
        b
    }
    fn echo_shape(s: Shape) -> Shape {
        s
    }
    fn echo_options(a: Option<String>, b: Option<Option<u32>>) -> (Option<String>, Option<Option<u32>>) {
        (a, b)
    }
    fn echo_flags(a: Small, b: Wide) -> (Small, Wide) {
        (a, b)
    }
    fn check(n: u64) -> Result<u64, Failure> {
        if n.is_multiple_of(2) { Ok(n) } else if n > 1000 { Err(Failure::TooBig(n)) } else { Err(Failure::BadInput(format!("{n} is odd"))) }
    }
    fn check_unit(fail: bool) -> Result<(), ()> {
        if fail { Err(()) } else { Ok(()) }
    }
    fn no_result(_n: u32) {}
    fn boom(msg: String) -> u32 {
        panic!("{msg}")
    }
    fn sum(xs: Vec<i64>) -> i64 {
        xs.iter().sum()
    }
    fn concat(xs: Vec<String>) -> String {
        xs.concat()
    }
    fn make_counters(n: u32) -> Vec<Counter> {
        (0..n).map(|i| Counter::new(MyCounter::new(i as i64))).collect()
    }
    fn consume(c: Counter) -> i64 {
        c.get::<MyCounter>().get()
    }
    fn emit(to: &Listener, events: Vec<String>) -> u32 {
        for e in &events {
            to.on_event(e);
        }
        to.count()
    }
}

impl GuestCounter for MyCounter {
    fn new(start: i64) -> Self {
        TOTAL.fetch_add(start, Ordering::Relaxed);
        MyCounter(AtomicI64::new(start))
    }
    fn add(&self, n: i64) -> i64 {
        TOTAL.fetch_add(n, Ordering::Relaxed);
        self.0.fetch_add(n, Ordering::Relaxed) + n
    }
    fn get(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
    fn merge(&self, other: CounterBorrow<'_>) {
        self.add(other.get::<MyCounter>().get());
    }
    fn total() -> i64 {
        TOTAL.load(Ordering::Relaxed)
    }
}

export!(Server);
