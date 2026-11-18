// SOLO: the version resource (product name Solo, Solo's version) that every exe
// Solo ships carries, see crates/windows_resources.
fn main() {
    #[cfg(not(target_os = "windows"))]
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rerun-if-env-changed=RELEASE_CHANNEL");
        println!("cargo:rerun-if-env-changed=GITHUB_RUN_NUMBER");
        println!("cargo:rerun-if-env-changed=SOLO_COMMIT_SHA");

        windows_resources::compile_with_description(false, Some("Solo agent hook"))
            .expect("failed to compile Windows resources");
    }
}
