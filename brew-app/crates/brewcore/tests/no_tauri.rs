//! The workspace split is only load-bearing if it is enforced. A `use tauri::...`
//! in `brewcore` would compile fine and quietly undo the reason the split exists,
//! so assert the dependency graph instead of trusting discipline.

use std::process::Command;

#[test]
fn brewcore_does_not_depend_on_tauri() {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--package",
            "brewcore",
            "--edges",
            "normal",
            "--prefix",
            "none",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run cargo tree");

    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let tree = String::from_utf8_lossy(&output.stdout);
    let offenders: Vec<&str> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| *name == "tauri" || name.starts_with("tauri-"))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    assert!(
        offenders.is_empty(),
        "brewcore gained a tauri dependency: {offenders:?}"
    );
}
