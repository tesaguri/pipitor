use std::fs;

use pipitor::Manifest;

#[test]
fn pipitor_example_toml() {
    let f = fs::read("Pipitor.example.toml").unwrap();
    toml::from_slice::<Manifest>(&f).unwrap();
    serde_dhall::from_file("Pipitor.example.dhall")
        .parse::<Manifest>()
        .unwrap();
}
