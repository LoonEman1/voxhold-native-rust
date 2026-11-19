//! Build metadata used by the update layer.
//!
//! The release workflow passes the published version and, for local acceptance,
//! a test public key. `option_env!` only sees a variable that changed when the
//! build script reruns, so both are declared here.

fn main() {
    println!("cargo:rerun-if-env-changed=VOXHOLD_BUILD_VERSION");
    println!("cargo:rerun-if-env-changed=VOXHOLD_UPDATE_PUBKEY");
}
