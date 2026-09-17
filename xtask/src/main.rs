use clap::{Parser, Subcommand};
use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::{env, fs};

const KEYS_DIR: &str = "keys";
const PRIVATE_KEY_FILE: &str = "douglas_signing.key";
const PUBLIC_KEY_FILE: &str = "douglas_signing.pub";
const TRAILER_MAGIC: &[u8; 8] = b"SAW_KERF";
const VERSION_LEN: usize = 3;
const SIGNATURE_LEN: usize = 64;
const TRAILER_LEN: usize = VERSION_LEN + SIGNATURE_LEN + TRAILER_MAGIC.len();

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Keygen,
    Sign {
        path: PathBuf,
    },
    Build {
        #[arg(long)]
        release: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Keygen => keygen(),
        Commands::Sign { path } => sign(&path),
        Commands::Build { release } => build(release),
    };

    if let Err(message) = result {
        eprintln!("{message}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

fn keygen() -> Result<(), String> {
    let keys_dir = Path::new(KEYS_DIR);
    fs::create_dir_all(keys_dir).map_err(|err| format!("failed to create {KEYS_DIR}: {err}"))?;

    let private_key_path = keys_dir.join(PRIVATE_KEY_FILE);
    let public_key_path = keys_dir.join(PUBLIC_KEY_FILE);

    if private_key_path.exists() {
        return Err(format!(
            "{} already exists, refusing to overwrite an existing signing key",
            private_key_path.display()
        ));
    }

    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();

    fs::write(&private_key_path, hex::encode(signing_key.to_bytes()))
        .map_err(|err| format!("failed to write {}: {err}", private_key_path.display()))?;
    fs::write(&public_key_path, hex::encode(verifying_key.to_bytes()))
        .map_err(|err| format!("failed to write {}: {err}", public_key_path.display()))?;

    println!("Generated a new signing keypair:");
    println!(
        "  private: {} (never commit this)",
        private_key_path.display()
    );
    println!("  public:  {}", public_key_path.display());
    Ok(())
}

fn load_signing_key() -> Result<SigningKey, String> {
    let private_key_path = Path::new(KEYS_DIR).join(PRIVATE_KEY_FILE);
    let encoded = fs::read_to_string(&private_key_path).map_err(|err| {
        format!(
            "failed to read {}: {err} (run `cargo run -p xtask -- keygen` first)",
            private_key_path.display()
        )
    })?;

    let bytes = hex::decode(encoded.trim())
        .map_err(|err| format!("{} is not valid hex: {err}", private_key_path.display()))?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
        format!(
            "{} does not contain a 32-byte key",
            private_key_path.display()
        )
    })?;

    Ok(SigningKey::from_bytes(&bytes))
}

#[derive(serde::Deserialize)]
struct CargoToml {
    package: CargoPackage,
}

#[derive(serde::Deserialize)]
struct CargoPackage {
    version: String,
}

fn read_douglas_version() -> Result<(u8, u8, u8), String> {
    let cargo_toml = fs::read_to_string("Cargo.toml")
        .map_err(|err| format!("failed to read Cargo.toml: {err}"))?;
    let parsed: CargoToml =
        toml::from_str(&cargo_toml).map_err(|err| format!("failed to parse Cargo.toml: {err}"))?;

    parse_version(&parsed.package.version)
}

fn parse_version(raw: &str) -> Result<(u8, u8, u8), String> {
    let mut parts = raw.split('.');
    let mut next = || -> Result<u8, String> {
        parts
            .next()
            .ok_or_else(|| format!("'{raw}' is not a MAJOR.MINOR.PATCH version"))?
            .parse::<u8>()
            .map_err(|err| format!("'{raw}' is not a valid version: {err}"))
    };
    Ok((next()?, next()?, next()?))
}

fn strip_existing_trailer(data: &mut Vec<u8>) {
    if data.len() < TRAILER_LEN {
        return;
    }
    let magic_start = data.len() - TRAILER_MAGIC.len();
    if data[magic_start..] == TRAILER_MAGIC[..] {
        data.truncate(data.len() - TRAILER_LEN);
    }
}

fn sign(path: &Path) -> Result<(), String> {
    let signing_key = load_signing_key()?;
    let (major, minor, patch) = read_douglas_version()?;

    let mut data =
        fs::read(path).map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    strip_existing_trailer(&mut data);

    data.push(major);
    data.push(minor);
    data.push(patch);

    let signature = signing_key.sign(&data);
    data.extend_from_slice(&signature.to_bytes());
    data.extend_from_slice(TRAILER_MAGIC);

    fs::write(path, data).map_err(|err| format!("failed to write {}: {err}", path.display()))?;

    println!(
        "Signed {} in place (v{major}.{minor}.{patch})",
        path.display()
    );
    Ok(())
}

fn target_dir() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"))
}

fn build(release: bool) -> Result<(), String> {
    let mut command = Command::new("cargo");
    command.args(["build", "--bin", "douglas"]);
    if release {
        command.arg("--release");
    }

    let status = command
        .status()
        .map_err(|err| format!("failed to run cargo build: {err}"))?;
    if !status.success() {
        return Err(format!("cargo build failed: {status}"));
    }

    let profile_dir = if release { "release" } else { "debug" };
    let binary_path = target_dir().join(profile_dir).join("douglas");
    sign(&binary_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version_should_split_major_minor_patch() {
        assert_eq!(parse_version("1.2.3").unwrap(), (1, 2, 3));
    }

    #[test]
    fn test_parse_version_should_reject_too_few_components() {
        assert!(parse_version("1.2").is_err());
    }

    #[test]
    fn test_parse_version_should_reject_non_numeric_components() {
        assert!(parse_version("1.2.beta").is_err());
    }

    #[test]
    fn test_parse_version_should_reject_a_component_too_large_for_u8() {
        assert!(parse_version("1.2.300").is_err());
    }

    fn signed_looking_bytes(payload: &[u8]) -> Vec<u8> {
        let mut data = payload.to_vec();
        data.extend_from_slice(&[1, 2, 3]); // version
        data.extend_from_slice(&[0u8; SIGNATURE_LEN]); // fake signature
        data.extend_from_slice(TRAILER_MAGIC);
        data
    }

    #[test]
    fn test_strip_existing_trailer_should_remove_a_well_formed_trailer() {
        let mut data = signed_looking_bytes(b"pretend binary bytes");
        let original_payload_len = b"pretend binary bytes".len();

        strip_existing_trailer(&mut data);

        assert_eq!(data, b"pretend binary bytes");
        assert_eq!(data.len(), original_payload_len);
    }

    #[test]
    fn test_strip_existing_trailer_should_leave_data_without_a_trailer_untouched() {
        let mut data = b"just a plain, unsigned binary".to_vec();
        let original = data.clone();

        strip_existing_trailer(&mut data);

        assert_eq!(data, original);
    }

    #[test]
    fn test_strip_existing_trailer_should_leave_data_too_short_for_a_trailer_untouched() {
        let mut data = vec![1, 2, 3];
        let original = data.clone();

        strip_existing_trailer(&mut data);

        assert_eq!(data, original);
    }

    #[test]
    fn test_strip_existing_trailer_should_leave_data_with_the_wrong_magic_untouched() {
        let mut data = signed_looking_bytes(b"pretend binary bytes");
        let last = data.len() - 1;
        data[last] ^= 0xFF;
        let original = data.clone();

        strip_existing_trailer(&mut data);

        assert_eq!(data, original);
    }

    #[test]
    fn test_sign_should_be_idempotent_when_called_twice() {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let mut data = b"pretend binary bytes".to_vec();

        strip_existing_trailer(&mut data);
        data.extend_from_slice(&[1, 2, 3]);
        let first_signature = signing_key.sign(&data);
        data.extend_from_slice(&first_signature.to_bytes());
        data.extend_from_slice(TRAILER_MAGIC);

        let signed_once_len = data.len();

        strip_existing_trailer(&mut data);
        data.extend_from_slice(&[1, 2, 3]);
        let second_signature = signing_key.sign(&data);
        data.extend_from_slice(&second_signature.to_bytes());
        data.extend_from_slice(TRAILER_MAGIC);

        assert_eq!(data.len(), signed_once_len);
    }
}
