// The conformance suite in C++: a port of the Rust client's `run_suite`.
#include <cmath>
#include <limits>
#include <stdexcept>

#include "client.hpp"

namespace t = wna::test::types;
using wna::test::sink::Listener;

#define EXPECT(c)                                                                                         \
  do {                                                                                                    \
    if (!(c)) throw std::runtime_error(std::string("line ") + std::to_string(__LINE__) + ": " #c);  \
  } while (0)

static t::Scalars scalars(int64_t seed) {
  return t::Scalars{seed % 2 == 0, 255, uint16_t(65535 - seed), UINT32_MAX, UINT64_MAX - uint64_t(seed), INT8_MIN, INT16_MIN,
                    int32_t(INT32_MIN + seed), INT64_MIN, std::numeric_limits<float>::min(), -0.0, U'🦀'};
}

static t::Bag bag(uint32_t n) {
  t::Bag b;
  b.name = "bag-" + std::to_string(n) + "-ünïcödé-🎒";
  for (uint32_t i = 0; i < n; i++) b.tags.push_back("t" + std::to_string(i));
  b.shapes = {t::Shape{t::shape::Circle{1.5}}, t::Shape{t::shape::Rect{{2.0, -3.0}}}, t::Shape{t::shape::Label{std::string(n, 'x')}},
              t::Shape{t::shape::Empty{}}};
  if (n % 2 == 0) b.color = t::Color::blue;
  if (n % 3 == 1) b.nested = std::optional<uint32_t>{};
  if (n % 3 == 2) b.nested = std::optional<uint32_t>{n};
  b.pair = {uint8_t(n), "", 0.25f};
  b.small_ = t::Small::a | t::Small::c;
  b.wide_ = ~t::Wide::none;
  for (uint32_t i = 0; i < n * 7; i++) b.bytes.push_back(uint8_t(i));
  return b;
}

void run_cpp_suite() {
  for (int64_t s = 0; s < 4; s++) EXPECT(t::echo_scalars(scalars(s)) == scalars(s));
  auto nan = scalars(0);
  nan.f64v = std::nan("");
  nan.f32v = std::numeric_limits<float>::infinity();
  auto back = t::echo_scalars(nan);
  EXPECT(std::isnan(back.f64v) && std::isinf(back.f32v));

  for (std::string s : {"", "a", "héllo wörld", "🦀🦀🦀"}) EXPECT(t::echo_string(s) == s);
  std::string z(1 << 16, 'z');
  EXPECT(t::echo_string(z) == z);
  EXPECT(t::echo_bytes({}).empty());
  std::vector<uint8_t> big(1'000'000);
  for (size_t i = 0; i < big.size(); i++) big[i] = uint8_t(i * 31);
  EXPECT(t::echo_bytes(big) == big);

  for (uint32_t n = 0; n < 5; n++) EXPECT(t::echo_bag(bag(n)) == bag(n));
  std::vector<t::Bag> bags;
  for (uint32_t n = 0; n < 20; n++) bags.push_back(bag(n));
  EXPECT(t::echo_bags(bags) == bags);
  EXPECT(t::echo_bags({}).empty());
  for (auto s : {t::Shape{t::shape::Circle{1e308}}, t::Shape{t::shape::Rect{{0, 1}}}, t::Shape{t::shape::Label{""}}, t::Shape{t::shape::Empty{}}})
    EXPECT(t::echo_shape(s) == s);

  using O = std::optional<std::optional<uint32_t>>;
  for (auto [a, b] : std::vector<std::pair<std::optional<std::string>, O>>{
           {std::nullopt, std::nullopt}, {"x", O{std::optional<uint32_t>{}}}, {"", O{std::optional<uint32_t>{7}}}}) {
    auto [ra, rb] = t::echo_options(a, b);
    EXPECT(ra == a && rb == b);
  }
  EXPECT(t::echo_flags(t::Small::none, t::Wide::none) == std::make_tuple(t::Small::none, t::Wide::none));
  EXPECT(t::echo_flags(t::Small::b, t::Wide::f31 | t::Wide::f0) == std::make_tuple(t::Small::b, t::Wide::f31 | t::Wide::f0));

  EXPECT(t::check(4).value() == 4);
  EXPECT(t::check(5).error() == t::Failure{t::failure::BadInput{"5 is odd"}});
  EXPECT(t::check(1001).error() == t::Failure{t::failure::TooBig{1001}});
  EXPECT(t::check_unit(false).has_value());
  EXPECT(!t::check_unit(true).has_value());
  t::no_result(1);
  try {
    t::boom("kaboom");
    EXPECT(false);
  } catch (const wna::trap& e) {
    EXPECT(e.code == WNA_TRAP_PANIC && std::string(e.what()).find("kaboom") != std::string::npos);
  }
  EXPECT(t::sum(std::vector<int64_t>{1, 2, 3, -4}) == 2);
  EXPECT(t::sum({}) == 0);
  EXPECT(t::concat(std::vector<std::string>{"a", "", "ç"}) == "aç");

  int64_t before = t::Counter::total();
  uint32_t stale;
  {
    t::Counter a(10);
    EXPECT(a.add(5) == 15);
    t::Counter b(100);
    a.merge(b);
    EXPECT(a.get() == 115);
    EXPECT(t::Counter::total() - before == 215);
    auto many = t::make_counters(3);
    EXPECT(many.size() == 3 && many[0].get() == 0 && many[2].get() == 2);
    EXPECT(t::consume(std::move(b)) == 100);
    stale = a.handle();
  }
  try {
    t::Counter::borrow(stale).get();
    EXPECT(false);
  } catch (const wna::trap& e) {
    EXPECT(e.code == WNA_TRAP_BAD_HANDLE);
  }

  Listener l;
  EXPECT(t::emit(l, std::vector<std::string>{"one", "two"}) == 2);
  EXPECT(t::emit(l, {}) == 2);
  EXPECT(l.count() == 2);
}

void bind_cpp_client(wna_iface types, wna_iface sink) {
  std::vector<wna_iface> v;
  for (const char* const* n = client::imports; *n; n++) v.push_back(std::string(*n).starts_with("wna:test/types") ? types : sink);
  client::bind(v);
}
