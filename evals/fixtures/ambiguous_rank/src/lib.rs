#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub name: String,
    pub age: u32,
}

/// Sort people for display. See docs/ — requirements are underspecified in README.
pub fn rank_people(people: &[Person]) -> Vec<Person> {
    // Broken: sorts by age only (ignores name).
    let mut out = people.to_vec();
    out.sort_by_key(|p| p.age);
    out
}
