// WNA runtime for C++20 (header-only). Generated bindings build on it.
#pragma once

#include <cstdlib>
#include <cstring>
#include <exception>
#include <memory>
#include <mutex>
#include <optional>
#include <span>
#include <stdexcept>
#include <string>
#include <string_view>
#include <tuple>
#include <utility>
#include <variant>
#include <vector>

#if __has_include(<expected>) && __cplusplus > 202002L
#include <expected>
#endif

#include "wna.h"

namespace wna {

#if defined(__cpp_lib_expected)
template <class T, class E> using expected = std::expected<T, E>;
template <class E> using unexpected = std::unexpected<E>;
#else
/// Minimal stand-in for std::expected before C++23.
template <class E> struct unexpected {
  E error;
  explicit unexpected(E e) : error(std::move(e)) {}
};
template <class T, class E> class expected {
  std::variant<T, E> v_;

 public:
  expected(T v) : v_(std::in_place_index<0>, std::move(v)) {}
  expected(unexpected<E> e) : v_(std::in_place_index<1>, std::move(e.error)) {}
  bool has_value() const { return v_.index() == 0; }
  explicit operator bool() const { return has_value(); }
  T& value() { return std::get<0>(v_); }
  const T& value() const { return std::get<0>(v_); }
  T& operator*() { return value(); }
  E& error() { return std::get<1>(v_); }
  const E& error() const { return std::get<1>(v_); }
  bool operator==(const expected&) const = default;
};
template <class E> class expected<void, E> {
  std::optional<E> e_;

 public:
  expected() = default;
  expected(unexpected<E> e) : e_(std::move(e.error)) {}
  bool has_value() const { return !e_; }
  explicit operator bool() const { return has_value(); }
  E& error() { return *e_; }
  const E& error() const { return *e_; }
  bool operator==(const expected&) const = default;
};
#endif

/// Tag for adopting an owned resource handle.
struct adopt_t {
  explicit adopt_t() = default;
};
inline constexpr adopt_t adopt{};

/// `result<_, _>`'s error type.
struct unit {
  bool operator==(const unit&) const = default;
};

/// A call that failed as a call. Domain errors come back as `expected`.
struct trap : std::runtime_error {
  int32_t code;
  trap(int32_t c, const std::string& m) : std::runtime_error(m), code(c) {}
};

namespace detail {

inline std::string& last_trap_message() {
  thread_local std::string m;
  return m;
}

/// The `last_trap` entry of every table C++ exports.
extern "C" inline void last_trap(void*, wna_string* msg) {
  auto& m = last_trap_message();
  msg->ptr = reinterpret_cast<uint8_t*>(m.data());
  msg->len = m.size();
}

inline std::string trap_message(wna_iface i, size_t offset) {
  auto* h = static_cast<const wna_table_header*>(i.table);
  if (!h) return "interface not bound";
  if (h->size < offset + sizeof(wna_fn)) return "the exporter's table predates this function";
  wna_string msg{nullptr, 0};
  if (h->last_trap) h->last_trap(i.inst, &msg);
  return std::string(reinterpret_cast<const char*>(msg.ptr), msg.len);
}

}  // namespace detail

/// Calls a slot; a nonzero status throws `wna::trap`.
inline void invoke(wna_iface i, size_t offset, const void* args, void* ret, const char* what) {
  int32_t st = wna_call(i, offset, args, ret, wna_libc_alloc());
  if (st != WNA_OK) throw trap(st, std::string(what) + ": " + detail::trap_message(i, offset));
}

/// Generation-checked resource handles for objects C++ exports.
template <class T> class handles {
  std::mutex mu_;
  std::vector<std::pair<uint8_t, std::shared_ptr<T>>> slots_;
  std::vector<uint32_t> free_;

 public:
  uint32_t insert(std::shared_ptr<T> o) {
    std::lock_guard g(mu_);
    uint32_t i;
    if (!free_.empty()) {
      i = free_.back();
      free_.pop_back();
    } else {
      i = static_cast<uint32_t>(slots_.size());
      slots_.emplace_back(0, nullptr);
    }
    auto& s = slots_[i];
    s.first = s.first == 255 ? 1 : s.first + 1;
    s.second = std::move(o);
    return uint32_t(s.first) << 24 | i;
  }
  std::shared_ptr<T> get(uint32_t h) {
    std::lock_guard g(mu_);
    uint32_t i = h & 0xFFFFFF;
    if (i >= slots_.size() || slots_[i].first != (h >> 24) || !slots_[i].second) throw trap(WNA_TRAP_BAD_HANDLE, "stale or foreign handle");
    return slots_[i].second;
  }
  std::shared_ptr<T> remove(uint32_t h) {
    auto o = get(h);
    std::lock_guard g(mu_);
    slots_[h & 0xFFFFFF].second = nullptr;
    free_.push_back(h & 0xFFFFFF);
    return o;
  }
};

/// Memory for one call. Import side: lowered parameters live in scratch
/// memory freed at scope exit, lifted results are freed after copying.
/// Export side: results are allocated through the caller's allocator.
class cx {
  const wna_alloc* alloc_ = nullptr;
  std::vector<void*> scratch_;
  std::vector<std::tuple<void*, size_t, size_t>> allocated_;

 public:
  const bool owned;
  void* exports = nullptr;  // the exporting box (resource tables)

  cx() : owned(true) {}
  cx(void* box, const wna_alloc* alloc) : alloc_(alloc), owned(false), exports(box) {}
  cx(const cx&) = delete;
  ~cx() {
    for (void* p : scratch_) std::free(p);
  }

  void* bytes(size_t size, size_t align) {
    if (size == 0) return nullptr;
    if (alloc_) {
      void* p = alloc_->realloc(alloc_->ctx, nullptr, 0, align, size);
      if (!p) throw trap(WNA_TRAP_OOM, "out of memory");
      allocated_.emplace_back(p, size, align);
      return p;
    }
    void* p = std::malloc(size);
    if (!p) throw trap(WNA_TRAP_OOM, "out of memory");
    scratch_.push_back(p);
    return p;
  }
  template <class C> C* array(size_t n) { return static_cast<C*>(bytes(n * sizeof(C), alignof(C))); }
  /// Releases what a failed export lowering allocated through the caller.
  void unwind() {
    if (!alloc_) return;
    for (auto [p, size, align] : allocated_) alloc_->realloc(alloc_->ctx, p, size, align, 0);
    allocated_.clear();
  }
  void release(void* p) {
    if (owned) std::free(p);
  }

  wna_string string(std::string_view s) {
    auto* p = static_cast<uint8_t*>(bytes(s.size(), 1));
    if (p) std::memcpy(p, s.data(), s.size());
    return {p, s.size()};
  }
  std::string string(const wna_string& s) {
    std::string out(reinterpret_cast<const char*>(s.ptr), s.len);
    release(s.ptr);
    return out;
  }
};

inline char32_t lift_char(uint32_t c) {
  if (c > 0x10FFFF || (c >= 0xD800 && c <= 0xDFFF)) throw trap(WNA_TRAP_INVALID, "invalid char");
  return static_cast<char32_t>(c);
}

/// Runs an export body: exceptions become statuses (boundaries never unwind).
template <class F> int32_t guard(void* box, const wna_alloc* alloc, F&& body) noexcept {
  cx c(box, alloc);
  try {
    body(c);
    return WNA_OK;
  } catch (const trap& t) {
    c.unwind();
    detail::last_trap_message() = t.what();
    return t.code;
  } catch (const std::exception& e) {
    c.unwind();
    detail::last_trap_message() = e.what();
    return WNA_TRAP_PANIC;
  } catch (...) {
    c.unwind();
    detail::last_trap_message() = "unknown exception";
    return WNA_TRAP_PANIC;
  }
}

/// Validates UTF-8 before a std::string crosses (the sender guarantees validity).
inline void check_utf8(std::string_view s) {
  size_t i = 0;
  auto bad = [] { throw trap(WNA_TRAP_INVALID, "string is not valid UTF-8"); };
  while (i < s.size()) {
    unsigned char c = s[i];
    size_t n = c < 0x80 ? 1 : (c >> 5) == 0x6 ? 2 : (c >> 4) == 0xE ? 3 : (c >> 3) == 0x1E ? 4 : 0;
    if (n == 0 || i + n > s.size()) bad();
    for (size_t k = 1; k < n; k++)
      if ((static_cast<unsigned char>(s[i + k]) >> 6) != 0x2) bad();
    i += n;
  }
}

}  // namespace wna
