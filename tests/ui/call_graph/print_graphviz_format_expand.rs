//@ print-call-graph
//@ stdout
//@ mutest-flags: --graph-format=graphviz --call-graph-non-local-calls=expand

fn by_fn_item(x: u8) -> u8 { x }

#[test]
fn test() {
    let _ = Some(1).map(by_fn_item);
}
