//! Loads every definition in a directory and reports which ones the engine can't parse.
//!
//! `cargo run -p reelix-indexers --example check_definitions -- <dir>`

use std::collections::BTreeMap;

use reelix_indexers::DefinitionStore;

fn main() {
    let dir = std::env::args().nth(1).expect("usage: check_definitions <dir>");
    let (store, errors) = DefinitionStore::load_dir(std::path::Path::new(&dir)).expect("readable dir");
    let loaded = store.list().len();
    println!("loaded {loaded}, failed {}", errors.len());
    // Group failures by the first line of the error so the common gaps stand out.
    let mut buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in &errors {
        let key = e
            .message
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(140)
            .collect::<String>();
        let name = e
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        buckets.entry(key).or_default().push(name);
    }
    let mut sorted: Vec<_> = buckets.into_iter().collect();
    sorted.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
    for (k, v) in sorted.iter().take(40) {
        println!(
            "{:4}  {k}   e.g. {}",
            v.len(),
            v.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
        );
    }
}
