//! Build metadata used by the update layer, plus the Windows resource file that
//! carries the product icon.
//!
//! The release workflow passes the published version and, for local acceptance,
//! a test public key. `option_env!` only sees a variable that changed when the
//! build script reruns, so both are declared here.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=VOXHOLD_BUILD_VERSION");
    println!("cargo:rerun-if-env-changed=VOXHOLD_UPDATE_PUBKEY");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_icon();
    }
}

/// Embeds `assets/voxhold.rc`, which references `assets/voxhold.ico`, into the
/// executable so Explorer, the taskbar and shortcuts show the branded icon
/// instead of the generic one.
///
/// The resource file names the icon without a directory; `rc.exe` resolves that
/// name through the include path, which is anchored at the crate directory.
fn embed_windows_icon() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let assets = manifest_dir.join("assets");
    let rc = assets.join("voxhold.rc");

    println!("cargo:rerun-if-changed={}", rc.display());
    println!(
        "cargo:rerun-if-changed={}",
        assets.join("voxhold.ico").display()
    );

    // `.manifest_optional()` because the icon is cosmetic: gpui already links
    // its own RT_MANIFEST with DPI awareness and Common-Controls 6.
    embed_resource::compile(&rc, embed_resource::ParamsIncludeDirs([&assets]))
        .manifest_optional()
        .expect("failed to embed the Windows product icon");
}
