//! Read cookies from installed browsers' on-disk cookie databases.
//!
//! Currently supports Chrome on macOS. Chrome encrypts cookie values
//! with AES-128-CBC using a key derived from the "Chrome Safe Storage"
//! password in the macOS Keychain.
//!
//! The user's normal browser handles login (Google OAuth works fine),
//! and we read the resulting session cookies directly from the DB.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use log::{info, warn};
use rusqlite::Connection;

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

/// Chrome's AES encryption constants on macOS.
const CHROME_SALT: &[u8] = b"saltysalt";
const CHROME_IV: &[u8; 16] = b"                "; // 16 spaces
const CHROME_ITERATIONS: u32 = 1003;
const CHROME_KEY_LEN: usize = 16;

/// Reads cookies for a given domain from Chrome's cookie database.
/// Returns a `Cookie`-header-style string (`name=value; name2=value2`),
/// or `None` if no relevant cookies were found.
///
/// Only reads cookies whose names appear in `allowed_names`. Pass an
/// empty slice to read all cookies for the domain.
///
/// Handles chunked cookies (e.g. `session-token.0`, `session-token.1`)
/// by reassembling them in order.
pub fn read_chrome_cookies(
    domain: &str,
    allowed_names: &[&str],
) -> Result<Option<String>, String> {
    let db_paths = chrome_cookie_db_paths();
    if db_paths.is_empty() {
        return Err("Chrome cookie database not found. Is Chrome installed?".to_string());
    }

    // Try each profile until we find cookies for this domain.
    for db_path in &db_paths {
        info!("trying Chrome profile: {}", db_path.display());

        // Chrome locks the DB while running. Copy to a secure temp file
        // (randomized name, 0600 permissions, auto-deleted on drop).
        let mut tmp = tempfile::NamedTempFile::new()
            .map_err(|e| format!("Failed to create temp file: {e}"))?;

        let db_bytes = std::fs::read(db_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                "Permission denied reading Chrome cookies. \
                 Grant Full Disk Access to usage-meter in \
                 System Settings > Privacy & Security."
                    .to_string()
            } else {
                format!("Failed to read cookie DB: {e}")
            }
        })?;

        tmp.write_all(&db_bytes)
            .map_err(|e| format!("Failed to write temp cookie DB: {e}"))?;

        match read_cookies_from_db(tmp.path(), domain, allowed_names)? {
            Some(cookies) => return Ok(Some(cookies)),
            None => {
                info!("no cookies for {domain} in {}", db_path.display());
                continue;
            }
        }
    }

    Ok(None)
}

/// Returns cookie DB paths for all Chrome profiles, checking Default
/// first, then Profile 1, Profile 2, etc.
fn chrome_cookie_db_paths() -> Vec<PathBuf> {
    let home = match std::env::var_os("HOME").map(PathBuf::from) {
        Some(h) => h,
        None => return vec![],
    };
    let chrome_dir = home
        .join("Library")
        .join("Application Support")
        .join("Google")
        .join("Chrome");

    let mut paths = Vec::new();

    // Check Default profile first.
    let default_cookies = chrome_dir.join("Default").join("Cookies");
    if default_cookies.exists() {
        paths.push(default_cookies);
    }

    // Check numbered profiles (Profile 1, Profile 2, …).
    if let Ok(entries) = std::fs::read_dir(&chrome_dir) {
        let mut profiles: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| n.starts_with("Profile "))
                    .unwrap_or(false)
            })
            .map(|e| e.path().join("Cookies"))
            .filter(|p| p.exists())
            .collect();
        profiles.sort();
        paths.extend(profiles);
    }

    info!("found {} Chrome cookie DB(s): {:?}", paths.len(), paths);
    paths
}

fn read_cookies_from_db(
    db_path: &Path,
    domain: &str,
    allowed_names: &[&str],
) -> Result<Option<String>, String> {
    let conn =
        Connection::open(db_path).map_err(|e| format!("Failed to open cookie DB: {e}"))?;

    // Query cookies matching the domain (both exact and dot-prefixed).
    let mut stmt = conn
        .prepare(
            "SELECT name, encrypted_value \
             FROM cookies \
             WHERE host_key = ?1 OR host_key = ?2 \
             ORDER BY name",
        )
        .map_err(|e| format!("SQL prepare failed: {e}"))?;

    let dot_domain = format!(".{domain}");
    let rows = stmt
        .query_map([domain, &dot_domain], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .map_err(|e| format!("SQL query failed: {e}"))?;

    let chrome_key = get_chrome_decryption_key()?;

    let mut cookies: HashMap<String, String> = HashMap::new();
    let mut chunked: HashMap<String, Vec<(u32, String)>> = HashMap::new();

    for row in rows {
        let (name, encrypted_value) = row.map_err(|e| format!("Row read failed: {e}"))?;

        // Filter to allowed cookie names if a whitelist is provided.
        if !allowed_names.is_empty() {
            let dominated = allowed_names.iter().any(|a| {
                name == *a || name.starts_with(&format!("{a}."))
            });
            if !dominated {
                continue;
            }
        }

        let value = decrypt_chrome_cookie(&encrypted_value, &chrome_key).unwrap_or_else(|e| {
            warn!("failed to decrypt cookie {name}: {e}");
            String::new()
        });

        if value.is_empty() {
            continue;
        }

        // Handle chunked cookies like `session-token.0`, `session-token.1`.
        if let Some((base, idx)) = parse_chunked_name(&name) {
            chunked.entry(base).or_default().push((idx, value));
        } else {
            cookies.insert(name, value);
        }
    }

    // Reassemble chunked cookies.
    for (base, mut chunks) in chunked {
        chunks.sort_by_key(|(idx, _)| *idx);
        let assembled: String = chunks.into_iter().map(|(_, v)| v).collect();
        cookies.insert(base, assembled);
    }

    if cookies.is_empty() {
        info!("no cookies found for {domain} in Chrome");
        return Ok(None);
    }

    info!(
        "read {} cookies for {domain} from Chrome: {:?}",
        cookies.len(),
        cookies.keys().collect::<Vec<_>>()
    );

    let header = cookies
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ");

    Ok(Some(header))
}

/// Parses `"foo.0"` -> `Some(("foo", 0))`, `"foo.12"` -> `Some(("foo", 12))`.
/// Returns `None` if the name doesn't end with `.<digits>`.
fn parse_chunked_name(name: &str) -> Option<(String, u32)> {
    let (base, suffix) = name.rsplit_once('.')?;
    let idx: u32 = suffix.parse().ok()?;
    Some((base.to_string(), idx))
}

/// Retrieves the Chrome Safe Storage password from the macOS Keychain
/// and derives the AES-128-CBC key using PBKDF2.
fn get_chrome_decryption_key() -> Result<[u8; CHROME_KEY_LEN], String> {
    let entry = keyring::Entry::new("Chrome Safe Storage", "Chrome")
        .map_err(|e| format!("Keychain entry error: {e}"))?;

    let password = entry
        .get_password()
        .map_err(|e| format!("Failed to read Chrome Safe Storage from Keychain: {e}"))?;

    let mut key = [0u8; CHROME_KEY_LEN];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(
        password.as_bytes(),
        CHROME_SALT,
        CHROME_ITERATIONS,
        &mut key,
    );

    Ok(key)
}

/// Decrypts a Chrome-encrypted cookie value.
/// Format: `v10` prefix (3 bytes) + AES-128-CBC ciphertext.
fn decrypt_chrome_cookie(
    encrypted: &[u8],
    key: &[u8; CHROME_KEY_LEN],
) -> Result<String, String> {
    if encrypted.is_empty() {
        return Ok(String::new());
    }

    // Chrome prefixes encrypted values with "v10" (macOS) or "v11".
    if encrypted.len() < 4 {
        return Err("encrypted value too short".to_string());
    }

    let prefix = &encrypted[..3];
    if prefix != b"v10" && prefix != b"v11" {
        // Might be an unencrypted value (older Chrome).
        return String::from_utf8(encrypted.to_vec())
            .map_err(|e| format!("not valid UTF-8: {e}"));
    }

    let ciphertext = &encrypted[3..];

    let decryptor = Aes128CbcDec::new(key.into(), CHROME_IV.into());
    let mut buf = ciphertext.to_vec();
    let plaintext = decryptor
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| format!("AES decryption failed: {e}"))?;

    String::from_utf8(plaintext.to_vec())
        .map_err(|e| format!("decrypted value not valid UTF-8: {e}"))
}
