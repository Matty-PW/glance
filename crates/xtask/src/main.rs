//! Developer tasks, run as `cargo xtask <task>`.
//!
//! `bundle`: build `glance` in release mode and assemble a signed
//! `target/bundle/Glance.app` (`docs/DESIGN.md` §10).
//!
//! Why a stable signature matters: macOS ties Screen Recording and
//! Microphone grants to the bundle ID plus the signature's designated
//! requirement. A certificate-based requirement stays the same across
//! rebuilds, so grants survive; an ad-hoc signature's requirement is the code
//! hash, which changes with every build, so macOS asks again each time.
//!
//! The signing identity comes from `GLANCE_SIGN_IDENTITY` (a name or SHA-1
//! from `security find-identity -v -p codesigning`) and is never committed.
//! Only std, `cargo`, and the Command Line Tools' `codesign` and `plutil`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const BUNDLE_ID: &str = "io.github.matty-pw.glance";
const EXECUTABLE: &str = "glance";
const MIN_MACOS: &str = "14.2";
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Shown by macOS in the microphone prompt.
const MIC_USAGE: &str = "glance listens only while you hold its hotkey, to hear your question.";

/// Hardened runtime blocks microphone access unless this entitlement is
/// present. Nothing else is needed: no JIT, no unsigned libraries, no
/// network-client entitlement (the app is not sandboxed).
const ENTITLEMENTS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>com.apple.security.device.audio-input</key>
	<true/>
</dict>
</plist>
"#;

fn info_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>{BUNDLE_ID}</string>
	<key>CFBundleName</key>
	<string>Glance</string>
	<key>CFBundleDisplayName</key>
	<string>Glance</string>
	<key>CFBundleExecutable</key>
	<string>{EXECUTABLE}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleShortVersionString</key>
	<string>{VERSION}</string>
	<key>CFBundleVersion</key>
	<string>{VERSION}</string>
	<key>LSMinimumSystemVersion</key>
	<string>{MIN_MACOS}</string>
	<key>LSUIElement</key>
	<true/>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSMicrophoneUsageDescription</key>
	<string>{MIC_USAGE}</string>
</dict>
</plist>
"#
    )
}

fn main() -> ExitCode {
    let result = match std::env::args().nth(1).as_deref() {
        Some("bundle") => bundle(),
        _ => {
            eprintln!(
                "usage: cargo xtask <task>\n\ntasks:\n  bundle    build and sign target/bundle/Glance.app"
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn bundle() -> Result<(), String> {
    let root = workspace_root();
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    run(Command::new(cargo).current_dir(&root).args([
        "build",
        "--release",
        "--locked",
        "-p",
        "glance",
    ]))?;

    // A fixed path, so the app macOS sees is always the same one.
    let app = target.join("bundle").join("Glance.app");
    if app.exists() {
        fs::remove_dir_all(&app).map_err(|e| format!("removing old {}: {e}", app.display()))?;
    }
    let contents = app.join("Contents");
    let macos = contents.join("MacOS");
    fs::create_dir_all(&macos).map_err(|e| format!("creating {}: {e}", macos.display()))?;
    let binary = target.join("release").join(EXECUTABLE);
    fs::copy(&binary, macos.join(EXECUTABLE))
        .map_err(|e| format!("copying {}: {e}", binary.display()))?;
    write(&contents.join("Info.plist"), &info_plist())?;
    run(Command::new("plutil")
        .arg("-lint")
        .arg(contents.join("Info.plist")))?;

    let entitlements = target.join("bundle").join("glance.entitlements");
    write(&entitlements, ENTITLEMENTS)?;

    let identity = std::env::var("GLANCE_SIGN_IDENTITY")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let sign_with = match &identity {
        Some(identity) => identity.as_str(),
        None => {
            eprintln!(
                "xtask: WARNING: GLANCE_SIGN_IDENTITY is not set, signing ad-hoc. macOS will ask \
                 for Screen Recording and Microphone again after every rebuild. Set it to an \
                 identity from `security find-identity -v -p codesigning`."
            );
            "-"
        }
    };
    // `--timestamp=none`: a secure timestamp would contact Apple's server,
    // and is only needed for notarized distribution.
    run(Command::new("codesign")
        .args(["--force", "--options", "runtime", "--timestamp=none"])
        .arg("--entitlements")
        .arg(&entitlements)
        .args(["--sign", sign_with])
        .arg(&app))?;
    run(Command::new("codesign")
        .args(["--verify", "--strict", "--verbose=1"])
        .arg(&app))?;

    println!("\nbuilt {}", app.display());
    println!(
        "designated requirement (grants are tied to this; it must not change between builds):"
    );
    run(Command::new("codesign")
        .args(["--display", "--requirements", "-"])
        .arg(&app))?;
    println!(
        "\nlaunch:\n  open -W --stderr \"$(tty)\" {} --args app --endpoint http://127.0.0.1:<port>/v1 --model <model>",
        app.display()
    );
    Ok(())
}

fn workspace_root() -> PathBuf {
    // crates/xtask -> workspace root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("xtask lives two levels below the workspace root")
        .to_path_buf()
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, text).map_err(|e| format!("writing {}: {e}", path.display()))
}

fn run(command: &mut Command) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|e| format!("could not run {:?}: {e}", command.get_program()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{:?} failed ({status})", command.get_program()))
    }
}
