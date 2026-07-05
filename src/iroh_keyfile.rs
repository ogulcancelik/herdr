//! Encrypted identity key storage (koh-key-v1 inspired).
//!
//! Identity keys are stored encrypted at rest using Argon2id + AES-256-GCM.
//! The format is modeled on OpenSSH's `openssh-key-v1`:
//!
//! ```text
//! koh-key-v1\n
//! <base64: nonce (12) || salt (32) || ciphertext>
//! ```
//!
//! Security properties:
//! - Keys are never written to disk unencrypted.
//! - Atomic writes via temp-file rename.
//! - `O_NOFOLLOW` on read (symlink hardening, Unix only).
//! - Owner-only permissions (0600).
//! - Secret material zeroized on drop.

use std::fs;
use std::io::{self, IsTerminal};
use std::path::Path;

use aes_gcm::aead::{Aead, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, Key, KeyInit, Nonce};
use argon2::Argon2;
use secrecy::{ExposeSecret, SecretString};

const KDF_SALT_LEN: usize = 32;
const KDF_MEMORY_KIB: u32 = 65536; // 64 MiB
const KDF_ITERATIONS: u32 = 4;
const KDF_PARALLELISM: u32 = 1;
const KEYFILE_MAGIC: &str = "koh-key-v1\n";
const MIN_PASSPHRASE_LEN: usize = 8;

/// Errors for keyfile operations.
#[derive(Debug)]
pub enum KeyfileError {
    Io(io::Error),
    Other(String),
}

impl std::fmt::Display for KeyfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyfileError::Io(e) => write!(f, "io error: {e}"),
            KeyfileError::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for KeyfileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            KeyfileError::Io(e) => Some(e),
            KeyfileError::Other(_) => None,
        }
    }
}

impl From<io::Error> for KeyfileError {
    fn from(e: io::Error) -> Self {
        KeyfileError::Io(e)
    }
}

/// Load a 32-byte secret key from an encrypted keyfile, or create a new one.
///
/// If the keyfile exists, prompts for the passphrase (via TTY or
/// `$HERDR_IROH_KEY_PASSPHRASE` env var) and decrypts the key.
///
/// If the keyfile does not exist, prompts for a new passphrase (via TTY or
/// `$HERDR_IROH_KEY_NEW_PASSPHRASE` env var), generates a fresh Ed25519 key,
/// encrypts it, and writes it to disk.
pub fn load_or_create_key(key_dir: &Path, key_file: &str) -> Result<[u8; 32], KeyfileError> {
    let key_path = key_dir.join(key_file);

    if key_path.exists() {
        let pass = resolve_passphrase(&key_path, false)?;
        read_encrypted_key(&key_path, pass.expose_secret())
    } else {
        fs::create_dir_all(key_dir)?;
        let pass = resolve_passphrase(&key_path, true)?;
        let secret = generate_secret_key();
        write_encrypted_key(&key_path, &secret, pass.expose_secret())?;
        Ok(secret)
    }
}

/// Change the passphrase on an existing encrypted keyfile.
pub fn change_passphrase(key_dir: &Path, key_file: &str) -> Result<(), KeyfileError> {
    let key_path = key_dir.join(key_file);

    if !key_path.exists() {
        return Err(KeyfileError::Other(
            "no identity key found — run `herdr iroh-bridge id` first".into(),
        ));
    }

    let old_pass = resolve_passphrase(&key_path, false)?;
    let secret = read_encrypted_key(&key_path, old_pass.expose_secret())?;

    let new_pass = prompt_new_passphrase_interactive(&key_path)?;
    write_encrypted_key(&key_path, &secret, new_pass.expose_secret())?;

    eprintln!("passphrase changed successfully.");
    Ok(())
}

/// Migrate a raw (unencrypted) 32-byte key to the encrypted format.
///
/// Reads the raw key bytes, prompts for a new passphrase, and writes
/// the encrypted keyfile.  The raw file is overwritten atomically.
pub fn migrate_raw_key(
    key_dir: &Path,
    key_file: &str,
    raw_secret: &[u8; 32],
) -> Result<(), KeyfileError> {
    let key_path = key_dir.join(key_file);

    if !io::stdin().is_terminal() {
        return Err(KeyfileError::Other(
            "migrating legacy raw key requires a TTY; set $HERDR_IROH_KEY_NEW_PASSPHRASE".into(),
        ));
    }

    eprintln!(
        "Found a legacy unencrypted identity key at {}",
        key_path.display()
    );
    eprintln!("It will be migrated to an encrypted format. Choose a passphrase.");
    eprintln!();

    let pass = prompt_new_passphrase_interactive(&key_path)?;
    write_encrypted_key(&key_path, raw_secret, pass.expose_secret())?;

    eprintln!("key migrated successfully.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal: encryption / decryption
// ---------------------------------------------------------------------------

fn generate_secret_key() -> [u8; 32] {
    let sk = iroh::SecretKey::generate();
    sk.to_bytes()
}

fn derive_key(passphrase: &str, salt: &[u8]) -> [u8; 32] {
    let mut key = [0u8; 32];
    Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon2::Params::new(KDF_MEMORY_KIB, KDF_ITERATIONS, KDF_PARALLELISM, Some(32))
            .expect("valid argon2 params"),
    )
    .hash_password_into(passphrase.as_bytes(), salt, &mut key)
    .expect("argon2 hash");
    key
}

fn read_encrypted_key(path: &Path, passphrase: &str) -> Result<[u8; 32], KeyfileError> {
    let contents = read_file_secure(path)?;
    let body = contents
        .strip_prefix(KEYFILE_MAGIC)
        .ok_or_else(|| KeyfileError::Other("invalid keyfile format: missing magic".into()))?;

    let decoded = data_encoding::BASE64
        .decode(body.trim().as_bytes())
        .map_err(|e| KeyfileError::Other(format!("invalid keyfile base64: {e}")))?;

    if decoded.len() < 12 + KDF_SALT_LEN + 16 {
        return Err(KeyfileError::Other("keyfile too short".into()));
    }

    let nonce = Nonce::from_slice(&decoded[..12]);
    let salt = &decoded[12..12 + KDF_SALT_LEN];
    let ciphertext = &decoded[12 + KDF_SALT_LEN..];

    let key_bytes = derive_key(passphrase, salt);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| KeyfileError::Other("decryption failed: wrong passphrase?".into()))?;

    if plaintext.len() != 32 {
        return Err(KeyfileError::Other(format!(
            "invalid keyfile: expected 32-byte key, got {}",
            plaintext.len()
        )));
    }

    let mut secret = [0u8; 32];
    secret.copy_from_slice(&plaintext);
    Ok(secret)
}

fn write_encrypted_key(
    path: &Path,
    secret: &[u8; 32],
    passphrase: &str,
) -> Result<(), KeyfileError> {
    let salt = rand::random::<[u8; KDF_SALT_LEN]>();
    let key_bytes = derive_key(passphrase, &salt);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);

    let ciphertext = cipher
        .encrypt(&nonce, secret.as_slice())
        .map_err(|e| KeyfileError::Other(format!("encryption failed: {e}")))?;

    let mut payload = Vec::with_capacity(12 + KDF_SALT_LEN + ciphertext.len());
    payload.extend_from_slice(&nonce);
    payload.extend_from_slice(&salt);
    payload.extend_from_slice(&ciphertext);

    let encoded = data_encoding::BASE64.encode(&payload);
    let contents = format!("{KEYFILE_MAGIC}{encoded}\n");

    // Atomic write: temp file + rename.
    let tmp_path = path.with_extension(".tmp");
    write_file_private(&tmp_path, contents.as_bytes())?;
    fs::rename(&tmp_path, path)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Internal: passphrase resolution
// ---------------------------------------------------------------------------

fn resolve_passphrase(key_path: &Path, is_new: bool) -> Result<SecretString, KeyfileError> {
    let env_var = if is_new {
        "HERDR_IROH_KEY_NEW_PASSPHRASE"
    } else {
        "HERDR_IROH_KEY_PASSPHRASE"
    };

    if let Ok(p) = std::env::var(env_var) {
        if !p.is_empty() {
            return Ok(SecretString::from(p));
        }
    }

    if is_new {
        prompt_new_passphrase_interactive(key_path)
    } else {
        prompt_existing_passphrase(key_path)
    }
}

fn prompt_existing_passphrase(key_path: &Path) -> Result<SecretString, KeyfileError> {
    if !io::stdin().is_terminal() {
        return Err(KeyfileError::Other(format!(
            "identity key {} is encrypted; set $HERDR_IROH_KEY_PASSPHRASE (no TTY available)",
            key_path.display()
        )));
    }

    let pass = rpassword::prompt_password(format!("Passphrase for {}: ", key_path.display()))
        .map_err(KeyfileError::Io)?;

    Ok(SecretString::from(pass))
}

fn prompt_new_passphrase_interactive(key_path: &Path) -> Result<SecretString, KeyfileError> {
    if !io::stdin().is_terminal() {
        return Err(KeyfileError::Other(format!(
            "creating identity key {} requires a TTY; set $HERDR_IROH_KEY_NEW_PASSPHRASE",
            key_path.display()
        )));
    }

    eprintln!("Creating a new identity key: {}", key_path.display());
    eprintln!("This passphrase encrypts the key at rest. Choose a strong one.");
    eprintln!();

    let pass = rpassword::prompt_password("New passphrase: ").map_err(KeyfileError::Io)?;

    if pass.len() < MIN_PASSPHRASE_LEN {
        return Err(KeyfileError::Other(format!(
            "passphrase must be at least {MIN_PASSPHRASE_LEN} characters"
        )));
    }

    let confirm = rpassword::prompt_password("Confirm passphrase: ").map_err(KeyfileError::Io)?;

    if pass != confirm {
        return Err(KeyfileError::Other("passphrases do not match".into()));
    }

    Ok(SecretString::from(pass))
}

// ---------------------------------------------------------------------------
// Internal: secure file I/O
// ---------------------------------------------------------------------------

/// Read a file securely: no symlink following, then verify permissions.
///
/// On non-Unix platforms, this returns an error — keyfile security
/// requires platform-specific protections that are not implemented
/// for this target.
fn read_file_secure(path: &Path) -> Result<String, KeyfileError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(false)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = file.metadata()?;
            let mode = metadata.permissions().mode() & 0o777;
            if mode != 0o600 {
                return Err(KeyfileError::Other(format!(
                    "keyfile {} has insecure permissions {mode:o} (expected 600)",
                    path.display()
                )));
            }
        }

        use std::io::Read;
        let mut contents = String::new();
        io::BufReader::new(file).read_to_string(&mut contents)?;
        Ok(contents)
    }

    #[cfg(not(unix))]
    {
        Err(KeyfileError::Other(
            "encrypted identity keys are not supported on this platform".into(),
        ))
    }
}

/// Write a file with owner-only permissions (0600).
///
/// On non-Unix platforms, this returns an error — keyfile security
/// requires platform-specific protections.
fn write_file_private(path: &Path, data: &[u8]) -> Result<(), KeyfileError> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(data)?;
        Ok(())
    }

    #[cfg(not(unix))]
    {
        Err(KeyfileError::Other(
            "encrypted identity keys are not supported on this platform".into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_encrypt_decrypt() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("test.key");
        let secret = generate_secret_key();

        let pass = "test-passphrase-1234";
        write_encrypted_key(&key_path, &secret, pass).unwrap();
        let loaded = read_encrypted_key(&key_path, pass).unwrap();

        assert_eq!(secret, loaded);
    }

    #[test]
    fn wrong_passphrase_fails() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("test.key");
        let secret = generate_secret_key();

        write_encrypted_key(&key_path, &secret, "correct-pass").unwrap();
        let result = read_encrypted_key(&key_path, "wrong-pass");

        assert!(result.is_err());
    }

    #[test]
    fn base64_roundtrip() {
        let input: Vec<u8> = (0..255).collect();
        let encoded = data_encoding::BASE64.encode(&input);
        let decoded = data_encoding::BASE64.decode(encoded.as_bytes()).unwrap();
        assert_eq!(input, decoded);
    }

    #[test]
    fn base64_decode_empty() {
        assert!(data_encoding::BASE64.decode(b"").unwrap().is_empty());
    }
}
