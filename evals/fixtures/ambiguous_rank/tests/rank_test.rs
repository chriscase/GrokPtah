use rank_lib::{rank_people, Person};

fn p(name: &str, age: u32) -> Person {
    Person {
        name: name.into(),
        age,
    }
}

#[test]
fn sorts_by_name_then_age() {
    let input = vec![
        p("Bob", 30),
        p("Alice", 40),
        p("Alice", 20),
        p("Carol", 25),
    ];
    let got = rank_people(&input);
    assert_eq!(
        got,
        vec![
            p("Alice", 20),
            p("Alice", 40),
            p("Bob", 30),
            p("Carol", 25),
        ]
    );
}

#[test]
fn empty_ok() {
    assert!(rank_people(&[]).is_empty());
}
