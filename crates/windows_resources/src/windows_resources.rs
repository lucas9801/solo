#![allow(
    clippy::disallowed_methods,
    reason = "build helper used only from build scripts"
)]
#![cfg(target_os = "windows")]

use std::process::Command;

fn git_sha() -> Option<String> {
    if let Ok(sha) = std::env::var("SOLO_COMMIT_SHA") {
        return Some(sha);
    }

    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

// SOLO: every exe Solo ships carries Solo's version, the version of the zed
// crate (crates/solo/Cargo.toml, which script/release-solo.ps1 sets), not the
// version of the crate it's built from (the CLI and the helpers are 0.1.0),
// so every exe of a release has the same product name and product version.
const SOLO_CARGO_TOML: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../solo/Cargo.toml");

/// The `version` of the `[package]` in crates/solo/Cargo.toml.
fn solo_version() -> String {
    println!("cargo:rerun-if-changed={SOLO_CARGO_TOML}");
    let manifest = std::fs::read_to_string(SOLO_CARGO_TOML)
        .unwrap_or_else(|error| panic!("can't read {SOLO_CARGO_TOML}: {error}"));
    parse_package_version(&manifest).unwrap_or_else(|| panic!("no version in {SOLO_CARGO_TOML}"))
}

fn parse_package_version(manifest: &str) -> Option<String> {
    manifest
        .lines()
        .take_while(|line| !line.starts_with('[') || line.trim() == "[package]")
        .find_map(|line| line.strip_prefix("version = \""))
        .and_then(|rest| rest.split('"').next())
        .map(str::to_string)
}

// SOLO: the build, "<version>+<channel>[.<run>][.<commit>]". It used to be the
// product version; it's the Comments now, so that the product version is the
// same on every build of a version.
fn build_metadata(pkg_version: &str) -> String {
    let commit_sha = git_sha();
    let channel = std::env::var("RELEASE_CHANNEL").unwrap_or_else(|_| "dev".into());
    let build_id = std::env::var("GITHUB_RUN_NUMBER").ok();

    let mut metadata = channel;
    if let Some(build_id) = &build_id {
        metadata.push('.');
        metadata.push_str(build_id);
    }
    if let Some(sha) = &commit_sha {
        metadata.push('.');
        metadata.push_str(sha);
    }

    format!("{pkg_version}+{metadata}")
}

// SOLO: the company and copyright every exe carries.
const COMPANY_NAME: &str = "lucas9801";
const LEGAL_COPYRIGHT: &str = "Copyright lucas9801 and Solo contributors. Based on Zed, Copyright 2022 - 2025 Zed Industries, Inc. GPL-3.0-or-later";

const ICON_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../solo/resources/windows");
const MANIFEST_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/manifest.xml");

pub fn compile(manifest: bool) -> Result<(), Box<dyn std::error::Error>> {
    compile_with_description(manifest, None)
}

/// SOLO: like [`compile`], with `description` as the file description (what
/// Task Manager and the file's details show); without one it's the product
/// name.
pub fn compile_with_description(
    manifest: bool,
    description: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let channel = option_env!("RELEASE_CHANNEL").unwrap_or("dev");
    let (icon_filename, product_name) = match channel {
        "stable" => ("app-icon.ico", "Solo"),
        "preview" => ("app-icon-preview.ico", "Solo Preview"),
        "nightly" => ("app-icon-nightly.ico", "Solo Nightly"),
        _ => ("app-icon-dev.ico", "Solo Dev"),
    };
    let icon = std::path::PathBuf::from(ICON_DIR).join(icon_filename);
    let icon_escaped = icon.to_string_lossy().replace('\\', "\\\\");

    let manifest_line = if manifest {
        let escaped = MANIFEST_PATH.replace('\\', "\\\\");
        format!("1 24 \"{escaped}\"")
    } else {
        String::new()
    };

    // SOLO: VS_FF_DEBUG on builds with debug assertions (the dev profile), so
    // packaging can tell them apart (script/package-solo-msix.ps1 refuses
    // them: they read assets and shaders from the source tree).
    let file_flags = if std::env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_some() {
        "0x1L"
    } else {
        "0x0L"
    };
    let pkg_version = solo_version();
    let build = build_metadata(&pkg_version);
    let description = description.unwrap_or(product_name);
    let mut version_parts = pkg_version
        .split('.')
        .map(|part| part.parse::<u16>().unwrap_or(0))
        .chain(std::iter::repeat(0));
    let file_version = format!(
        "{},{},{},{}",
        version_parts.next().unwrap_or(0),
        version_parts.next().unwrap_or(0),
        version_parts.next().unwrap_or(0),
        version_parts.next().unwrap_or(0),
    );

    // The resource paths can contain non-ASCII characters (e.g. when the
    // repository lives under a path with such characters). Rust Strings are
    // encoded using UTF-8, but rc.exe decodes the .rc file using the ANSI code
    // page by default, which mangles those paths and makes it fail to find the
    // referenced files, so specify UTF-8 decoding.
    let rc_content = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon_escaped}"
{manifest_line}

1 VERSIONINFO
FILEVERSION {file_version}
PRODUCTVERSION {file_version}
FILEFLAGSMASK 0x3fL
FILEFLAGS {file_flags}
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "FileDescription", "{description}\0"
            VALUE "FileVersion", "{pkg_version}\0"
            VALUE "ProductName", "{product_name}\0"
            VALUE "ProductVersion", "{pkg_version}\0"
            VALUE "Comments", "{build}\0"
            VALUE "CompanyName", "{COMPANY_NAME}\0"
            VALUE "LegalCopyright", "{LEGAL_COPYRIGHT}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#
    );

    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    let rc_path = out_dir.join("zed_resources.rc");
    std::fs::write(&rc_path, rc_content)?;

    if let Ok(toolkit_path) = std::env::var("SOLO_RC_TOOLKIT_PATH") {
        let rc_exe = std::path::Path::new(&toolkit_path).join("rc.exe");
        unsafe {
            std::env::set_var("RC", rc_exe);
        }
    }

    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_optional()
        .unwrap();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_package_version;

    #[test]
    fn reads_the_package_version() {
        let manifest = "[package]\nname = \"zed\"\ndescription = \"x\"\nversion = \"2.3.0\"\n\n[dependencies]\nversion = \"9.9.9\"\n";
        assert_eq!(parse_package_version(manifest).as_deref(), Some("2.3.0"));
        assert_eq!(
            parse_package_version("[dependencies]\nversion = \"1.0.0\"\n"),
            None
        );
    }

    #[test]
    fn reads_solo_version() {
        let manifest = std::fs::read_to_string(super::SOLO_CARGO_TOML).unwrap();
        let version = parse_package_version(&manifest).unwrap();
        assert_eq!(version.split('.').count(), 3, "{version}");
    }
}
