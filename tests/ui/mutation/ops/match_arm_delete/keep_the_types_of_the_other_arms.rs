//@ print-mutations
//@ build
//@ stdout
//@ stderr: empty
//@ mutation-operators: match_arm_delete
//@ edition: 2024

use std::borrow::Cow;

fn render<'a>(name: &str, repo: &'a str, root: &'a Cow<'a, str>) -> &'a str {
    // TEST: The first arm sets the type `&str`, to which the other arms coerce.
    let value = match name {
        "repo" => repo,
        "root" => &root,
        _ => "",
    };
    value
}

fn guarded(input: u8, limit: u8) -> u8 {
    match input {
        // TEST: The arm uses the name that its `let` guard binds.
        input if let Some(value) = input.checked_sub(limit) => value,
        _ => 0,
    }
}

#[test]
fn the_arms_keep_their_types() {
    assert_eq!(render("repo", "scm", &Cow::Borrowed("/")), "scm");
    assert_eq!(guarded(9, 4), 5);
}
