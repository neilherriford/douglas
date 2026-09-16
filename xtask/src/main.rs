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
const SIGNATURE_LEN: usize = 64;
const TRAILER_LEN: usize = SIGNATURE_LEN + TRAILER_MAGIC.len();

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

fn strip_existing_trailer(data: &mut Vec<u8>) {
    if data.len() < TRAILER_LEN {
        return;
    }
    let split_at = data.len() - TRAILER_LEN;
    if data[split_at + SIGNATURE_LEN..] == TRAILER_MAGIC[..] {
        data.truncate(split_at);
    }
}

fn sign(path: &Path) -> Result<(), String> {
    let signing_key = load_signing_key()?;

    let mut data =
        fs::read(path).map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    strip_existing_trailer(&mut data);

    let signature = signing_key.sign(&data);
    data.extend_from_slice(&signature.to_bytes());
    data.extend_from_slice(TRAILER_MAGIC);

    fs::write(path, data).map_err(|err| format!("failed to write {}: {err}", path.display()))?;

    println!("Signed {} in place", path.display());
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
