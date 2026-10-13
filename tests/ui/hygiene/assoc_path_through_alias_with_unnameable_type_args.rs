//@ build
//@ stderr: empty
//@ aux-build: metric_alias.rs

//! This test ensures that a type-relative path to an associated item through a type alias keeps the alias
//! when no path names a generic argument of the aliased type here, because the self type would otherwise be ambiguous.

#![allow(unused)]

extern crate metric_alias;

use metric_alias::GaugeVec;

#[test]
fn test() {
    // TEST: Inherent associated function, with a function of the same name in an impl for another generic argument.
    let counters = metric_alias::CounterVec::new(3);
    assert_eq!(counters.len(), 3);

    // TEST: Trait associated function, with the self type known only from the path.
    assert_eq!(GaugeVec::default().len(), 0);

    // TEST: Trait associated function through the alias as a qualified self type.
    assert_eq!(<metric_alias::CounterVec as Default>::default().len(), 0);
}
