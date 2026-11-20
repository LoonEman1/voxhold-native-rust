//! Rebuild the update library when the test public key changes.
//!
//! `verify::embedded_public_key` reads `VOXHOLD_UPDATE_PUBKEY` with
//! `option_env!`, which Cargo does not track on its own. A local acceptance
//! build with a test key therefore has to declare the variable here.

fn main() {
    println!("cargo:rerun-if-env-changed=VOXHOLD_UPDATE_PUBKEY");
}
