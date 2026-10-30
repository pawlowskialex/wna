// `wna:test/types` in C++: the same behaviour as the Rust reference server.
#include <atomic>
#include <numeric>

#include "server.hpp"

namespace t = wna::test::types;

namespace {

std::atomic<int64_t> total{0};

struct Counter : t::CounterResource {
  std::atomic<int64_t> v;
  explicit Counter(int64_t s) : v(s) { total += s; }
  int64_t add(int64_t n) override {
    total += n;
    return v += n;
  }
  int64_t get() override { return v; }
  void merge(t::CounterResource& other) override { add(other.get()); }
};

struct Server : t::Exports {
  t::Scalars echo_scalars(t::Scalars s) override { return s; }
  std::string echo_string(std::string s) override { return s; }
  std::vector<uint8_t> echo_bytes(std::vector<uint8_t> b) override { return b; }
  t::Bag echo_bag(t::Bag b) override { return b; }
  std::vector<t::Bag> echo_bags(std::vector<t::Bag> b) override { return b; }
  t::Shape echo_shape(t::Shape s) override { return s; }
  std::tuple<std::optional<std::string>, std::optional<std::optional<uint32_t>>> echo_options(
      std::optional<std::string> a, std::optional<std::optional<uint32_t>> b) override {
    return {a, b};
  }
  std::tuple<t::Small, t::Wide> echo_flags(t::Small a, t::Wide b) override { return {a, b}; }
  wna::expected<uint64_t, t::Failure> check(uint64_t n) override {
    if (n % 2 == 0) return n;
    if (n > 1000) return wna::unexpected(t::Failure{t::failure::TooBig{n}});
    return wna::unexpected(t::Failure{t::failure::BadInput{std::to_string(n) + " is odd"}});
  }
  wna::expected<void, wna::unit> check_unit(bool fail) override {
    if (fail) return wna::unexpected(wna::unit{});
    return {};
  }
  void no_result(uint32_t) override {}
  uint32_t boom(std::string msg) override { throw std::runtime_error(msg); }
  int64_t sum(std::vector<int64_t> xs) override { return std::accumulate(xs.begin(), xs.end(), int64_t{0}); }
  std::string concat(std::vector<std::string> xs) override { return std::accumulate(xs.begin(), xs.end(), std::string{}); }
  std::shared_ptr<t::CounterResource> counter_new(int64_t start) override { return std::make_shared<Counter>(start); }
  int64_t counter_total() override { return total; }
  std::vector<std::shared_ptr<t::CounterResource>> make_counters(uint32_t n) override {
    std::vector<std::shared_ptr<t::CounterResource>> v;
    for (uint32_t i = 0; i < n; i++) v.push_back(std::make_shared<Counter>(i));
    return v;
  }
  int64_t consume(std::shared_ptr<t::CounterResource> c) override { return c->get(); }
  uint32_t emit(const wna::test::sink::Listener& to, std::vector<std::string> events) override {
    for (auto& e : events) to.on_event(e);
    return to.count();
  }
};

}  // namespace

wna_iface cpp_server(wna_iface sink) {
  server::bind(std::span<const wna_iface>(&sink, 1));
  return t::table(std::make_shared<Server>());
}
