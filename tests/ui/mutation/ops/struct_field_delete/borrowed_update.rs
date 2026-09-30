//@ build
//@ stderr: empty
//@ mutation-operators: struct_field_delete

struct Record {
    text: String,
    number: u8,
    inherited: bool,
}

fn borrowed(base: &Record) -> Record {
    Record { text: String::new(), number: 9, ..*base }
}

fn owned(base: Record) -> Record {
    Record { text: String::new(), number: 9, ..base }
}

fn reused(base: Record) -> (Record, String) {
    let changed = Record { text: String::new(), number: 9, ..base };
    (changed, base.text)
}

#[test]
fn updates_do_not_move_borrowed_fields() {
    let base = Record { text: String::from("retained"), number: 4, inherited: true };
    let changed = borrowed(&base);
    assert_eq!(changed.text, "");
    assert_eq!(changed.number, 9);
    assert!(changed.inherited);
    assert_eq!(base.text, "retained");
    let changed = owned(base);
    assert_eq!(changed.text, "");
    assert_eq!(changed.number, 9);
    assert!(changed.inherited);
    let (changed, retained) = reused(Record { text: String::from("still owned"), number: 5, inherited: true });
    assert_eq!(changed.text, "");
    assert_eq!(changed.number, 9);
    assert!(changed.inherited);
    assert_eq!(retained, "still owned");
}
