mod common;

/// The iOS execution mode: the same component, interpreted.
#[test]
fn wasm_server_pulley() {
    common::run_against_wasm(wna_host::Execution::Pulley);
}
