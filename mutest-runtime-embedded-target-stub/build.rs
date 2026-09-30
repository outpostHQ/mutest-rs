#[path = "../build_support/runtime_contract.rs"]
mod runtime_contract;

fn main() {
    runtime_contract::enforce();
}
