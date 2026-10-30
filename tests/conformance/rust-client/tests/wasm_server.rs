mod common;

#[test]
fn wasm_server_cranelift() {
    common::run_against_wasm(wna_host::Execution::Native);
}
