//@ print-mutations
//@ build
//@ stdout
//@ stderr: empty
//@ mutation-operators: continue_break_swap

fn fields(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c != '{' {
            continue;
        }
        let mut field = String::new();
        for c in chars.by_ref() {
            if c == '}' {
                // A `continue` would take `field` round the loop after it has moved.
                out.push(field);
                break;
            }
            field.push(c);
        }
    }
    out
}

fn short_words_before_a_long_one(words: &[String]) -> usize {
    let mut count = 0;
    for word in words {
        if word.len() > 3 {
            break;
        }
        count += 1;
    }
    count
}

#[test]
fn test() {
    assert_eq!(fields("a {b} c {d}"), ["b", "d"]);
    assert_eq!(short_words_before_a_long_one(&["ab".to_owned(), "abcd".to_owned()]), 1);
}
