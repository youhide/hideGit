//! Embeds the icon and version metadata into the Windows executable.
//!
//! A window icon set at runtime only reaches the window itself. The icon
//! Explorer draws on `hidegit.exe`, and the one the taskbar and Alt-Tab use,
//! comes from a resource linked into the binary — which is what this does.
//!
//! Everything here is a no-op off Windows, including when cross-compiling to
//! Windows from a host without a resource compiler: `embed-resource` reports
//! that as `NotAttempted`, and a missing icon is not worth failing a build over.

use std::{env, fs, path::PathBuf};

use embed_resource::CompilationResult;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .parent()
        .and_then(|crates| crates.parent())
        .expect("crates/hidegit/ is always two levels below the workspace root");

    let template = root.join("packaging/windows/hidegit.rc.in");
    let icon = root.join("assets/generated/hidegit.ico");

    println!("cargo:rerun-if-changed={}", template.display());
    println!("cargo:rerun-if-changed={}", icon.display());

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let version = env!("CARGO_PKG_VERSION");

    // VERSIONINFO wants four comma-separated integers, so "0.1.0" becomes
    // "0,1,0,0". Padding rather than parsing keeps this honest if the version
    // ever grows a pre-release suffix.
    let mut parts: Vec<&str> = version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .take(4)
        .collect();
    parts.resize(4, "0");

    let script = fs::read_to_string(&template)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", template.display()))
        // Backslashes are escape characters inside an .rc string literal, so
        // the Windows path has to be doubled up.
        .replace(
            "@ICON_PATH@",
            &icon.display().to_string().replace('\\', "\\\\"),
        )
        .replace("@VERSION_COMMAS@", &parts.join(","))
        .replace("@VERSION@", version);

    let generated = PathBuf::from(env::var_os("OUT_DIR").expect("cargo always sets OUT_DIR"))
        .join("hidegit.rc");
    fs::write(&generated, script).expect("could not write the generated resource script");

    // Matched explicitly rather than via `manifest_optional()`, which treats
    // `NotAttempted` — no resource compiler on the machine — as success. That
    // would drop the icon silently, and silence is indistinguishable from a
    // build that worked. We are already known to be targeting Windows here, so
    // anything short of `Ok` is worth saying out loud.
    match embed_resource::compile(&generated, embed_resource::NONE) {
        CompilationResult::Ok | CompilationResult::NotWindows => {}
        other => println!("cargo:warning=the Windows icon was not embedded: {other:?}"),
    }
}
