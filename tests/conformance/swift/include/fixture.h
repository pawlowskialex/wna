#include "wna.h"

void wna_fixture_rust_sink(wna_iface *out);
void wna_fixture_rust_server(wna_iface sink, wna_iface *out);
int32_t wna_fixture_run_rust_client(wna_iface types, wna_iface sink, wna_string *msg);
