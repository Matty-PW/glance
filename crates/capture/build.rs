//! Help the linker find the Swift back-compatibility libraries that the
//! `screencapturekit` Swift bridge needs.
//!
//! `screencapturekit`'s own build script only looks inside a full Xcode
//! install (`<developer dir>/Toolchains/XcodeDefault.xctoolchain/...`). With
//! only the Command Line Tools installed, the same libraries live under
//! `<developer dir>/usr/lib/swift/macosx`, and linking fails with undefined
//! `__swift_FORCE_LOAD_$_swiftCompatibility56`. This adds whichever of the two
//! locations exists. Nothing is downloaded; only local paths are probed.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=DEVELOPER_DIR");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let developer_dir = std::env::var("DEVELOPER_DIR").ok().or_else(|| {
        let out = Command::new("xcode-select").arg("-p").output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    });
    let Some(developer_dir) = developer_dir else {
        println!("cargo:warning=could not locate the Apple developer tools (xcode-select -p)");
        return;
    };
    let root = PathBuf::from(developer_dir);
    for candidate in [
        root.join("Toolchains/XcodeDefault.xctoolchain/usr/lib/swift/macosx"),
        root.join("usr/lib/swift/macosx"),
    ] {
        if candidate.join("libswiftCompatibility56.a").is_file() {
            println!("cargo:rustc-link-search=native={}", candidate.display());
        }
    }
}
