//! How this glance is signed, for the setup window's permissions page:
//! macOS ties permission grants to the signing identity, so an ad-hoc
//! signature (a new identity on every build) loses them at each rebuild,
//! and a binary outside an app bundle has its permissions go to the
//! terminal that started it. Read with `/usr/bin/codesign -dv`; its output
//! (which names the certificate's owner) is parsed here and never logged.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signing {
    /// A real certificate: grants survive rebuilds.
    Stable,
    /// Ad hoc: grants are lost at every rebuild.
    AdHoc,
    /// Not inside Glance.app (`cargo run`): permissions belong to the
    /// terminal app.
    NotBundled,
    Unknown,
}

/// The `Glance.app` this executable is inside, if any.
pub fn own_bundle() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let text = exe.to_string_lossy().into_owned();
    let at = text.find(".app/Contents/MacOS/")?;
    Some(std::path::PathBuf::from(&text[..at + 4]))
}

/// How this process is signed. Runs `codesign` once; call off the hot path.
pub fn signing() -> Signing {
    let Some(bundle) = own_bundle() else {
        return Signing::NotBundled;
    };
    let out = std::process::Command::new("/usr/bin/codesign")
        .args(["-dv", "--verbose=2"])
        .arg(&bundle)
        .output();
    match out {
        // codesign writes the details to stderr.
        Ok(out) => parse_codesign(&String::from_utf8_lossy(&out.stderr)),
        Err(_) => Signing::Unknown,
    }
}

fn parse_codesign(text: &str) -> Signing {
    let lines = || text.lines().map(str::trim);
    if lines().any(|l| l == "Signature=adhoc") {
        Signing::AdHoc
    } else if lines().any(|l| l.starts_with("Authority=")) {
        Signing::Stable
    } else {
        Signing::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codesign_output_is_read() {
        let adhoc = "Executable=/x/Glance.app/Contents/MacOS/glance\nIdentifier=io.github.x.glance\n\
                     CodeDirectory v=20500 size=1 flags=0x10002(adhoc,runtime)\nSignature=adhoc\n";
        assert_eq!(parse_codesign(adhoc), Signing::AdHoc);
        let stable = "Identifier=io.github.x.glance\nAuthority=Apple Development: Someone (ABC)\n\
                      Authority=Apple Worldwide Developer Relations Certification Authority\n";
        assert_eq!(parse_codesign(stable), Signing::Stable);
        assert_eq!(
            parse_codesign("code object is not signed at all\n"),
            Signing::Unknown
        );
    }
}
