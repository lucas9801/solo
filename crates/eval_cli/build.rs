fn main() {
    let cargo_toml = std::fs::read_to_string("../solo/Cargo.toml")
        .expect("Failed to read crates/solo/Cargo.toml");
    let version = cargo_toml
        .lines()
        .find(|line| line.starts_with("version = "))
        .expect("Version not found in crates/solo/Cargo.toml")
        .split('=')
        .nth(1)
        .expect("Invalid version format")
        .trim()
        .trim_matches('"');
    println!("cargo:rerun-if-changed=../solo/Cargo.toml");
    println!("cargo:rustc-env=SOLO_PKG_VERSION={}", version);
}
