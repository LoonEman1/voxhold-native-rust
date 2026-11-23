//! Embed `assets/voxhold.ico` as resource id 1 for the helper window.
//!
//! The resource is cosmetic (window and taskbar icon), so a missing manifest
//! must not fail the build. Resource compilation only runs for Windows targets;
//! embed-resource skips other hosts.

fn main() {
    println!("cargo:rerun-if-changed=voxhold.rc");
    println!("cargo:rerun-if-changed=../../assets/voxhold.ico");
    embed_resource::compile(
        "voxhold.rc",
        embed_resource::ParamsIncludeDirs(&["../../assets"]),
    )
    .manifest_optional()
    .expect("embed the update helper icon");
}
