// C++ <-> Rust conformance, both directions, plus C++ <-> C++.
#include <cstdio>
#include <exception>
#include <stdexcept>
#include <string>

#include "wna.h"

extern "C" {
void wna_fixture_rust_sink(wna_iface* out);
void wna_fixture_rust_server(wna_iface sink, wna_iface* out);
int32_t wna_fixture_run_rust_client(wna_iface types, wna_iface sink, wna_string* msg);
}
wna_iface cpp_server(wna_iface sink);
void run_cpp_suite();
void bind_cpp_client(wna_iface types, wna_iface sink);

static bool run(const char* name, void (*f)(wna_iface, wna_iface), wna_iface a, wna_iface b) {
  try {
    f(a, b);
    std::printf("ok    %s\n", name);
    return true;
  } catch (const std::exception& e) {
    std::printf("FAIL  %s: %s\n", name, e.what());
    return false;
  }
}

int main() {
  wna_iface sink, rust_types;
  wna_fixture_rust_sink(&sink);
  wna_fixture_rust_server(sink, &rust_types);
  wna_iface cpp_types = cpp_server(sink);

  auto client = [](wna_iface types, wna_iface sink) {
    bind_cpp_client(types, sink);
    run_cpp_suite();
  };
  auto rust_client = [](wna_iface types, wna_iface sink) {
    wna_string msg{};
    if (wna_fixture_run_rust_client(types, sink, &msg) != 0) throw std::runtime_error(std::string((const char*)msg.ptr, msg.len));
  };
  bool ok = run("c++ client -> rust server", client, rust_types, sink);
  ok = run("rust client -> c++ server", rust_client, cpp_types, sink) && ok;
  ok = run("c++ client -> c++ server", client, cpp_types, sink) && ok;
  return ok ? 0 : 1;
}
