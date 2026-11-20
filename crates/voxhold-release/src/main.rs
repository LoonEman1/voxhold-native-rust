//! Release tooling for Voxhold updates.
//!
//! Commands:
//!
//! ```text
//! keygen  --out <pkcs8.b64>
//! pack    --dir <dir> --version <v> --out <zip>
//! unpack  --zip <zip> --out <dir>
//! delta   --base <zip> --target <zip> --out <delta.zip>
//! manifest --full <zip> --setup <exe> --version <v> --notes-file <file> --out <dir>
//!          [--channel beta] [--base-url <url>] [--min-windows 10.0.19041]
//!          [--published-at <rfc3339>] [--delta <delta.zip>]...
//!          [--authenticode] [--key-file <pkcs8.b64>]
//! verify  --manifest <latest.json> --sig <latest.json.sig>
//!          [--pubkey <base64>] [--pubkey-file <file>]
//!          [--full <zip>] [--setup <exe>] [--delta <delta.zip>]...
//! ```
//!
//! The signing key comes from `VOXHOLD_UPDATE_SIGNING_KEY` (base64 PKCS#8) or
//! `--key-file`. A missing key is an error: the tool never publishes an
//! unsigned manifest.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use semver::Version;
use voxhold_update::manifest::{DeltaEntry, FullPackage, Manifest, SetupPackage, BETA_CHANNEL};
use voxhold_update::package::{self, sha256_hex};
use voxhold_update::timefmt::format_rfc3339;
use voxhold_update::urls::validate_update_url;
use voxhold_update::verify::{self, UpdatePublicKey};

/// Base of the pinned GitHub release assets.
const DEFAULT_RELEASE_BASE: &str =
    "https://github.com/LoonEman1/voxhold-native-rust/releases/download";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("voxhold-release: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: Vec<String>) -> Result<(), String> {
    let mut arguments = arguments.into_iter();
    let command = arguments.next().ok_or_else(usage)?;
    let options = Options::parse(arguments)?;
    match command.as_str() {
        "keygen" => keygen(&options),
        "pack" => pack(&options),
        "unpack" => unpack(&options),
        "delta" => delta(&options),
        "manifest" => manifest(&options),
        "verify" => verify(&options),
        "--help" | "-h" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command {other}\n{USAGE}")),
    }
}

const USAGE: &str = "commands: keygen, pack, unpack, delta, manifest, verify";

fn usage() -> String {
    USAGE.to_string()
}

struct Options {
    values: HashMap<String, Vec<String>>,
    flags: HashMap<String, bool>,
}

impl Options {
    fn parse(arguments: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut values: HashMap<String, Vec<String>> = HashMap::new();
        let mut flags = HashMap::new();
        let mut arguments = arguments;
        while let Some(argument) = arguments.next() {
            if !argument.starts_with("--") {
                return Err(format!("unexpected argument {argument}"));
            }
            let name = argument.trim_start_matches("--").to_string();
            if matches!(name.as_str(), "authenticode" | "help") {
                flags.insert(name, true);
                continue;
            }
            let value = arguments
                .next()
                .ok_or_else(|| format!("missing value for --{name}"))?;
            values.entry(name).or_default().push(value);
        }
        Ok(Self { values, flags })
    }

    fn one(&self, name: &str) -> Result<&str, String> {
        self.values
            .get(name)
            .and_then(|values| values.last())
            .map(String::as_str)
            .ok_or_else(|| format!("missing --{name}"))
    }

    fn optional(&self, name: &str) -> Option<&str> {
        self.values
            .get(name)
            .and_then(|values| values.last())
            .map(String::as_str)
    }

    fn many(&self, name: &str) -> &[String] {
        self.values.get(name).map(Vec::as_slice).unwrap_or_default()
    }

    fn flag(&self, name: &str) -> bool {
        self.flags.get(name).copied().unwrap_or(false)
    }
}

fn keygen(options: &Options) -> Result<(), String> {
    let out = PathBuf::from(options.one("out")?);
    let (private, public) = verify::generate_keypair().map_err(|error| error.to_string())?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    std::fs::write(&out, format!("{private}\n"))
        .map_err(|error| format!("{}: {error}", out.display()))?;
    println!("public_key={public}");
    println!("private_key_file={}", out.display());
    eprintln!("warning: keep the private key outside the repository and never log it");
    Ok(())
}

fn pack(options: &Options) -> Result<(), String> {
    let dir = PathBuf::from(options.one("dir")?);
    let version = options.one("version")?;
    let out = PathBuf::from(options.one("out")?);
    let packed = package::pack_with_index(&dir, version).map_err(|error| error.to_string())?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    std::fs::write(&out, &packed.bytes).map_err(|error| format!("{}: {error}", out.display()))?;
    println!("sha256={}", packed.sha256);
    println!("size={}", packed.size());
    println!("files={}", packed.index.files.len());
    Ok(())
}

fn unpack(options: &Options) -> Result<(), String> {
    let zip = PathBuf::from(options.one("zip")?);
    let out = PathBuf::from(options.one("out")?);
    let bytes = std::fs::read(&zip).map_err(|error| format!("{}: {error}", zip.display()))?;
    let index = package::unpack(&bytes, &out).map_err(|error| error.to_string())?;
    println!("version={}", index.version);
    println!("files={}", index.files.len());
    Ok(())
}

fn delta(options: &Options) -> Result<(), String> {
    let base =
        std::fs::read(options.one("base")?).map_err(|error| format!("base package: {error}"))?;
    let target = std::fs::read(options.one("target")?)
        .map_err(|error| format!("target package: {error}"))?;
    let out = PathBuf::from(options.one("out")?);
    let bytes = voxhold_update::delta::create(&base, &target).map_err(|error| error.to_string())?;
    let manifest =
        voxhold_update::delta::read_manifest(&bytes).map_err(|error| error.to_string())?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    std::fs::write(&out, &bytes).map_err(|error| format!("{}: {error}", out.display()))?;
    println!("sha256={}", sha256_hex(&bytes));
    println!("size={}", bytes.len());
    println!("from_version={}", manifest.from_version);
    println!("to_version={}", manifest.to_version);
    println!("base_package_sha256={}", manifest.base_package_sha256);
    println!("target_package_sha256={}", manifest.target_package_sha256);
    Ok(())
}

fn manifest(options: &Options) -> Result<(), String> {
    let full_path = PathBuf::from(options.one("full")?);
    let setup_path = PathBuf::from(options.one("setup")?);
    let version = options.one("version")?.to_string();
    Version::parse(&version).map_err(|_| format!("invalid version {version}"))?;
    let notes_path = PathBuf::from(options.one("notes-file")?);
    let out = PathBuf::from(options.one("out")?);
    let channel = options
        .optional("channel")
        .unwrap_or(BETA_CHANNEL)
        .to_string();
    let min_windows = options
        .optional("min-windows")
        .unwrap_or("10.0.19041")
        .to_string();
    let authenticode = options.flag("authenticode");
    let published_at = match options.optional("published-at") {
        Some(value) => value.to_string(),
        None => format_rfc3339(now_seconds()),
    };
    let base = base_url(options)?;

    let full_bytes =
        std::fs::read(&full_path).map_err(|error| format!("{}: {error}", full_path.display()))?;
    let full_index = package::read_package_json(&full_bytes).map_err(|error| error.to_string())?;
    if full_index.version != version {
        return Err(format!(
            "full package version {} does not match {version}",
            full_index.version
        ));
    }
    let full_sha = sha256_hex(&full_bytes);
    let setup_bytes =
        std::fs::read(&setup_path).map_err(|error| format!("{}: {error}", setup_path.display()))?;

    let notes = std::fs::read_to_string(&notes_path)
        .map_err(|error| format!("{}: {error}", notes_path.display()))?
        .trim_start_matches('\u{feff}')
        .to_string();

    let mut deltas = Vec::new();
    for path in options.many("delta") {
        let bytes = std::fs::read(path).map_err(|error| format!("{path}: {error}"))?;
        let delta =
            voxhold_update::delta::read_manifest(&bytes).map_err(|error| error.to_string())?;
        if delta.to_version != version || delta.target_package_sha256 != full_sha {
            return Err(format!(
                "delta {path} does not reproduce the portable package for {version}"
            ));
        }
        deltas.push(DeltaEntry {
            url: asset_url(&base, &format!("VoxHold-delta-{}.zip", delta.from_version))?,
            from_version: delta.from_version,
            from_package_sha256: delta.base_package_sha256,
            size: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
        });
    }

    let manifest = Manifest {
        schema: voxhold_update::SCHEMA_VERSION,
        channel: channel.clone(),
        version,
        min_windows,
        published_at,
        notes,
        authenticode,
        full: FullPackage {
            url: asset_url(&base, "VoxHold-portable.zip")?,
            size: full_bytes.len() as u64,
            sha256: full_sha.clone(),
            package_sha256: full_sha,
        },
        setup: SetupPackage {
            url: asset_url(&base, "VoxHold-Setup.exe")?,
            size: setup_bytes.len() as u64,
            sha256: sha256_hex(&setup_bytes),
        },
        deltas,
    };
    manifest.validate().map_err(|error| error.to_string())?;

    let key = signing_key(options)?;
    let json = manifest.to_pretty_json();
    let signature = verify::sign_bytes(&key, &json).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&out).map_err(|error| format!("{}: {error}", out.display()))?;
    let manifest_path = out.join("latest.json");
    let signature_path = out.join("latest.json.sig");
    std::fs::write(&manifest_path, &json)
        .map_err(|error| format!("{}: {error}", manifest_path.display()))?;
    std::fs::write(&signature_path, format!("{signature}\n"))
        .map_err(|error| format!("{}: {error}", signature_path.display()))?;

    println!("version={}", manifest.version);
    println!("channel={}", manifest.channel);
    println!("schema={}", manifest.schema);
    println!("full_sha256={}", manifest.full.sha256);
    println!("setup_sha256={}", manifest.setup.sha256);
    println!("deltas={}", manifest.deltas.len());
    println!("manifest={}", manifest_path.display());
    println!("signature={}", signature_path.display());
    Ok(())
}

fn verify(options: &Options) -> Result<(), String> {
    let manifest_path = PathBuf::from(options.one("manifest")?);
    let signature_path = PathBuf::from(options.one("sig")?);
    let bytes = std::fs::read(&manifest_path)
        .map_err(|error| format!("{}: {error}", manifest_path.display()))?;
    let signature = std::fs::read_to_string(&signature_path)
        .map_err(|error| format!("{}: {error}", signature_path.display()))?;
    let key = match (options.optional("pubkey"), options.optional("pubkey-file")) {
        (Some(text), _) => UpdatePublicKey::from_base64(text),
        (None, Some(path)) => {
            let text = std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?;
            UpdatePublicKey::from_base64(&text)
        }
        (None, None) => verify::embedded_public_key(),
    }
    .map_err(|error| error.to_string())?;

    let manifest = verify::verify_and_parse(&key, &bytes, &signature)
        .map_err(|error| format!("{}: {error}", error.code()))?;
    check_artifact(
        options.optional("full"),
        manifest.full.size,
        &manifest.full.sha256,
        "full",
    )?;
    check_artifact(
        options.optional("setup"),
        manifest.setup.size,
        &manifest.setup.sha256,
        "setup",
    )?;
    for path in options.many("delta") {
        let bytes = std::fs::read(path).map_err(|error| format!("{path}: {error}"))?;
        let delta =
            voxhold_update::delta::read_manifest(&bytes).map_err(|error| error.to_string())?;
        let expected = manifest
            .deltas
            .iter()
            .find(|entry| {
                entry.from_version == delta.from_version
                    && entry.from_package_sha256 == delta.base_package_sha256
            })
            .ok_or_else(|| format!("delta {path} is not listed in the manifest"))?;
        if expected.size != bytes.len() as u64 || expected.sha256 != sha256_hex(&bytes) {
            return Err(format!("delta {path} does not match the manifest"));
        }
    }
    println!("signature=valid");
    println!("version={}", manifest.version);
    println!("channel={}", manifest.channel);
    println!("schema={}", manifest.schema);
    println!("min_windows={}", manifest.min_windows);
    println!("authenticode={}", manifest.authenticode);
    Ok(())
}

fn check_artifact(
    path: Option<&str>,
    expected_size: u64,
    expected_sha: &str,
    label: &str,
) -> Result<(), String> {
    let Some(path) = path else {
        return Ok(());
    };
    let bytes = std::fs::read(path).map_err(|error| format!("{path}: {error}"))?;
    if bytes.len() as u64 != expected_size || sha256_hex(&bytes) != expected_sha {
        return Err(format!(
            "{label} artifact {path} does not match the manifest"
        ));
    }
    println!("{label}_artifact=valid");
    Ok(())
}

fn signing_key(options: &Options) -> Result<String, String> {
    if let Some(path) = options.optional("key-file") {
        return std::fs::read_to_string(path)
            .map(|text| text.trim().to_string())
            .map_err(|error| format!("{path}: {error}"));
    }
    match std::env::var("VOXHOLD_UPDATE_SIGNING_KEY") {
        Ok(value) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        _ => Err("set VOXHOLD_UPDATE_SIGNING_KEY (base64 PKCS#8) or pass --key-file".into()),
    }
}

fn base_url(options: &Options) -> Result<String, String> {
    let base = match options.optional("base-url") {
        Some(value) => value.trim().trim_end_matches('/').to_string(),
        None => {
            let channel = options.optional("channel").unwrap_or(BETA_CHANNEL);
            format!("{DEFAULT_RELEASE_BASE}/{channel}")
        }
    };
    validate_update_url(&base).map_err(|error| format!("invalid --base-url: {}", error.code()))?;
    Ok(base)
}

fn asset_url(base: &str, asset: &str) -> Result<String, String> {
    let url = format!("{}/{asset}", base.trim_end_matches('/'));
    validate_update_url(&url).map_err(|error| format!("invalid {asset} URL: {}", error.code()))?;
    Ok(url)
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(values: &[(&str, &str)]) -> Options {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for (name, value) in values {
            map.entry((*name).to_string())
                .or_default()
                .push((*value).to_string());
        }
        Options {
            values: map,
            flags: HashMap::new(),
        }
    }

    #[test]
    fn base_url_accepts_https_and_loopback_http() {
        let opts = options(&[("base-url", "http://127.0.0.1:8123/")]);
        assert_eq!(base_url(&opts).unwrap(), "http://127.0.0.1:8123");
        let url = asset_url("http://127.0.0.1:8123", "VoxHold-portable.zip").unwrap();
        assert_eq!(url, "http://127.0.0.1:8123/VoxHold-portable.zip");

        let opts = options(&[("base-url", "https://example.invalid/assets/")]);
        assert_eq!(
            asset_url(&base_url(&opts).unwrap(), "latest.json").unwrap(),
            "https://example.invalid/assets/latest.json"
        );
    }

    #[test]
    fn base_url_rejects_remote_http_and_other_schemes() {
        for value in ["http://example.invalid/assets", "ftp://127.0.0.1/assets"] {
            let opts = options(&[("base-url", value)]);
            assert!(base_url(&opts).is_err(), "{value}");
        }
    }

    #[test]
    fn manifest_uses_the_base_url_and_still_signs() {
        let temp = tempfile::tempdir().unwrap();
        let payload = temp.path().join("payload");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join("Voxhold.exe"), b"binary").unwrap();
        let full = temp.path().join("VoxHold-portable.zip");
        std::fs::write(&full, package::pack(&payload, "0.2.0").unwrap()).unwrap();
        let setup = temp.path().join("VoxHold-Setup.exe");
        std::fs::write(&setup, b"setup").unwrap();
        let notes = temp.path().join("notes.md");
        std::fs::write(&notes, "Что нового").unwrap();
        let out = temp.path().join("dist");
        let (private, public) = verify::generate_keypair().unwrap();
        let key_file = temp.path().join("key.b64");
        std::fs::write(&key_file, private).unwrap();

        run(vec![
            "manifest".into(),
            "--full".into(),
            full.to_string_lossy().into_owned(),
            "--setup".into(),
            setup.to_string_lossy().into_owned(),
            "--version".into(),
            "0.2.0".into(),
            "--notes-file".into(),
            notes.to_string_lossy().into_owned(),
            "--out".into(),
            out.to_string_lossy().into_owned(),
            "--base-url".into(),
            "http://127.0.0.1:8123".into(),
            "--key-file".into(),
            key_file.to_string_lossy().into_owned(),
        ])
        .unwrap();

        let bytes = std::fs::read(out.join("latest.json")).unwrap();
        let signature = std::fs::read_to_string(out.join("latest.json.sig")).unwrap();
        let key = UpdatePublicKey::from_base64(&public).unwrap();
        let manifest = verify::verify_and_parse(&key, &bytes, &signature).unwrap();
        assert_eq!(manifest.version, "0.2.0");
        assert_eq!(
            manifest.full.url,
            "http://127.0.0.1:8123/VoxHold-portable.zip"
        );
        assert_eq!(
            manifest.setup.url,
            "http://127.0.0.1:8123/VoxHold-Setup.exe"
        );
    }
}
