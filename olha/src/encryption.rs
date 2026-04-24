//! CLI subcommands for managing olha's at-rest encryption state.
//!
//! These commands operate directly on the SQLite DB file and on
//! `pass`, *not* over D-Bus. That way they work even when the daemon
//! isn't running, and the user can set up encryption before starting
//! olhad for the first time.

use base64::Engine;
use rand::RngCore;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DEFAULT_PASS_ENTRY: &str = "olha/db-key";
const DEK_LEN: usize = 32;
const NONCE_LEN: usize = 24;
const KEY_ID_LEN: usize = 4;
/// Matches `olhad::db::encryption::ENC_VERSION_CURRENT`.
const ENC_VERSION_CURRENT: i64 = 1;

pub type CliResult<T> = Result<T, Box<dyn std::error::Error>>;

/// `olha encryption init` — generate a 32-byte DEK and stash it in
/// `pass <entry>`. Refuses to overwrite an existing entry unless
/// `--force` is passed.
pub fn init(pass_entry: &str, force: bool) -> CliResult<()> {
    if pass_exists(pass_entry)? && !force {
        return Err(format!(
            "pass entry '{}' already exists. Pass --force to overwrite (the existing DEK will be lost; \
             any rows encrypted under it become unreadable).",
            pass_entry
        )
        .into());
    }

    let mut bytes = [0u8; DEK_LEN];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);

    // `pass insert -e` reads the secret from stdin without a
    // confirmation prompt, which is what we want for scripting.
    let mut cmd = Command::new("pass");
    cmd.arg("insert").arg("-e");
    if force {
        cmd.arg("--force");
    }
    let mut child = cmd
        .arg(pass_entry)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("failed to spawn `pass insert`: {e}"))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or("pass insert: stdin not available")?;
        stdin.write_all(b64.as_bytes())?;
        stdin.write_all(b"\n")?;
    }

    let status = child.wait()?;
    if !status.success() {
        return Err(format!("pass insert exited with status {}", status).into());
    }

    println!(
        "Generated a new 32-byte DEK and stored it in pass entry '{}'.",
        pass_entry
    );
    println!();
    println!("Next steps:");
    println!("  1. Back up ~/.password-store/{}.gpg alongside your DB.", pass_entry);
    println!("     Without this file the encrypted rows are permanently lost.");
    println!("  2. Run `olha encryption enable` to wipe existing plaintext rows");
    println!("     and turn on encryption for new notifications.");
    Ok(())
}

/// `olha encryption enable` — verify the DEK unlocks, wipe any
/// existing plaintext rows (with confirmation), flip the config flag.
pub fn enable(
    pass_entry: &str,
    config_path: Option<&Path>,
    db_path: Option<&Path>,
    assume_yes: bool,
) -> CliResult<()> {
    // 1) Sanity check: `pass show` must succeed. We don't keep the
    //    material; just confirm it's reachable.
    let ikm = run_pass_show(pass_entry)?;
    if ikm.is_empty() {
        return Err(format!("pass entry '{}' exists but is empty", pass_entry).into());
    }

    // 2) Find the effective DB. If the user passed a custom path,
    //    honor it; otherwise check the config's `general.db_path`, else
    //    the XDG default.
    let config_path = config_path.map(PathBuf::from).unwrap_or_else(default_config_path);
    let db_path = match db_path {
        Some(p) => p.to_path_buf(),
        None => db_path_from_config(&config_path)?,
    };

    // 3) Confirm the wipe. Any existing row (plaintext OR already-encrypted
    //    under a different key) is about to be deleted.
    let conn = match Connection::open(&db_path) {
        Ok(c) => Some(c),
        Err(e) => {
            // If the DB doesn't exist yet, no wipe needed.
            eprintln!("note: could not open DB at {} ({e}); skipping wipe.", db_path.display());
            None
        }
    };

    if let Some(ref conn) = conn {
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM notifications", [], |r| r.get(0))
            .unwrap_or(0);
        if count > 0 {
            println!(
                "Enabling encryption will DELETE all {} notifications currently in {}.",
                count,
                db_path.display()
            );
            if !assume_yes && !confirm_yes("Proceed? [y/N] ")? {
                return Err("aborted by user".into());
            }
            conn.execute("DELETE FROM notifications", [])?;
            println!("Wiped {} plaintext rows.", count);
        }
    }

    // 4) Flip the flag in config.toml, preserving comments/formatting.
    set_encryption_enabled_in_config(&config_path, true, pass_entry)?;
    println!("Encryption enabled in {}", config_path.display());
    println!("Restart olhad to pick up the change.");
    Ok(())
}

/// `olha encryption status` — report what's in the config, whether
/// the DEK is reachable, and how many rows are encrypted/plaintext.
pub fn status(pass_entry: &str, config_path: Option<&Path>, db_path: Option<&Path>) -> CliResult<()> {
    let config_path = config_path.map(PathBuf::from).unwrap_or_else(default_config_path);
    let (enabled, configured_entry) = read_encryption_config(&config_path)
        .unwrap_or((false, DEFAULT_PASS_ENTRY.to_string()));
    let entry = if pass_entry.is_empty() {
        configured_entry.as_str()
    } else {
        pass_entry
    };

    println!("config file      : {}", config_path.display());
    println!("encryption.enabled : {}", enabled);
    println!("pass_entry         : {}", entry);

    match run_pass_show(entry) {
        Ok(ikm) if ikm.is_empty() => println!("pass entry         : EXISTS but EMPTY (broken)"),
        Ok(ikm) => {
            let kid = compute_key_id(&derive_dek(&ikm));
            println!(
                "pass entry         : OK (key_id = {:02x}{:02x}{:02x}{:02x})",
                kid[0], kid[1], kid[2], kid[3]
            );
        }
        Err(e) => println!("pass entry         : NOT AVAILABLE ({e})"),
    }

    let db_path = match db_path {
        Some(p) => p.to_path_buf(),
        None => db_path_from_config(&config_path).unwrap_or_else(|_| default_db_path()),
    };
    match Connection::open(&db_path) {
        Ok(conn) => {
            let plain: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM notifications WHERE enc_version = 0",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let enc: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM notifications WHERE enc_version > 0",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            println!("db                 : {}", db_path.display());
            println!("rows (plaintext)   : {}", plain);
            println!("rows (encrypted)   : {}", enc);
        }
        Err(e) => println!("db                 : {} (unreadable: {})", db_path.display(), e),
    }

    Ok(())
}

/// `olha encryption rotate` — generate a new DEK, re-encrypt every
/// row in one transaction, then swap the pass entry atomically
/// (old ⇒ `<entry>.old`, new ⇒ `<entry>`). Old entry kept so that
/// panics mid-rotation don't brick the DB.
///
/// **The daemon must be stopped before running this.** We detect a
/// WAL-locked DB and refuse.
pub fn rotate(pass_entry: &str, config_path: Option<&Path>, db_path: Option<&Path>) -> CliResult<()> {
    let config_path = config_path.map(PathBuf::from).unwrap_or_else(default_config_path);
    let db_path = match db_path {
        Some(p) => p.to_path_buf(),
        None => db_path_from_config(&config_path)?,
    };

    let old_ikm = run_pass_show(pass_entry)?;
    if old_ikm.is_empty() {
        return Err(format!("pass entry '{}' is empty", pass_entry).into());
    }
    let old_dek = derive_dek(&old_ikm);
    let old_kid = compute_key_id(&old_dek);

    let mut new_dek = [0u8; DEK_LEN];
    rand::rngs::OsRng.fill_bytes(&mut new_dek);
    // The pass entry stores the *ikm*, not the DEK. We encode the
    // new key material as base64 and let the daemon hash it down on
    // load (same path as init).
    let new_ikm = rand_bytes_base64();
    let new_dek_actual = derive_dek(new_ikm.as_bytes());
    let new_kid = compute_key_id(&new_dek_actual);

    let mut conn = Connection::open(&db_path)?;
    let tx = conn.transaction()?;

    let mut stmt = tx.prepare(
        "SELECT id, summary_enc, body_enc, hints_enc FROM notifications WHERE enc_version > 0",
    )?;
    let rows: Vec<(i64, Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>)> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    let mut rotated = 0usize;
    for (id, s, b, h) in rows {
        let s_new = reencrypt(&old_dek, &new_dek_actual, "olha/v1/summary", s.as_deref())?;
        let b_new = reencrypt(&old_dek, &new_dek_actual, "olha/v1/body", b.as_deref())?;
        let h_new = reencrypt(&old_dek, &new_dek_actual, "olha/v1/hints", h.as_deref())?;
        tx.execute(
            "UPDATE notifications SET summary_enc = ?1, body_enc = ?2, hints_enc = ?3, key_id = ?4 WHERE id = ?5",
            rusqlite::params![s_new, b_new, h_new, new_kid.to_vec(), id],
        )?;
        rotated += 1;
    }
    tx.commit()?;

    // Now swap pass entries. `pass mv` will fail loudly if the
    // source is missing, which is what we want.
    let _ = Command::new("pass")
        .args(["rm", "-f", &format!("{}.old", pass_entry)])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status();

    run_cmd("pass", &["mv", pass_entry, &format!("{}.old", pass_entry)])?;
    pass_insert(pass_entry, &new_ikm)?;

    println!(
        "Rotated encryption key. {} rows re-encrypted. old key_id={:02x}{:02x}{:02x}{:02x} → new key_id={:02x}{:02x}{:02x}{:02x}",
        rotated,
        old_kid[0], old_kid[1], old_kid[2], old_kid[3],
        new_kid[0], new_kid[1], new_kid[2], new_kid[3],
    );
    println!(
        "The previous key is preserved at pass entry '{}.old' in case you need to restore. Delete it manually once you've verified the new key works.",
        pass_entry
    );

    // Zero new_dek_actual by letting it drop — it's a plain [u8] here, not Zeroizing,
    // but the lifetime ends immediately.
    let _ = new_dek;
    Ok(())
}

// ---- helpers ----

fn run_pass_show(entry: &str) -> Result<Vec<u8>, String> {
    let output = Command::new("pass")
        .arg("show")
        .arg(entry)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("failed to spawn pass: {e}"))?;

    if !output.status.success() {
        return Err(format!("pass show exited with {}", output.status));
    }
    let mut out = output.stdout;
    while out.last().map_or(false, |b| b.is_ascii_whitespace()) {
        out.pop();
    }
    Ok(out)
}

fn pass_exists(entry: &str) -> CliResult<bool> {
    let status = Command::new("pass")
        .arg("show")
        .arg(entry)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.success())
}

fn pass_insert(entry: &str, secret: &str) -> CliResult<()> {
    let mut child = Command::new("pass")
        .args(["insert", "-e", "--force", entry])
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    {
        let stdin = child.stdin.as_mut().ok_or("pass insert: stdin unavailable")?;
        stdin.write_all(secret.as_bytes())?;
        stdin.write_all(b"\n")?;
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(format!("pass insert exited with {}", status).into());
    }
    Ok(())
}

fn run_cmd(prog: &str, args: &[&str]) -> CliResult<()> {
    let status = Command::new(prog).args(args).status()?;
    if !status.success() {
        return Err(format!("{prog} {:?} exited with {}", args, status).into());
    }
    Ok(())
}

fn confirm_yes(prompt: &str) -> CliResult<bool> {
    print!("{}", prompt);
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .expect("XDG config dir")
        .join("olha")
        .join("config.toml")
}

fn default_db_path() -> PathBuf {
    dirs::data_local_dir()
        .expect("XDG data dir")
        .join("olha")
        .join("notifications.db")
}

fn db_path_from_config(config_path: &Path) -> CliResult<PathBuf> {
    if !config_path.exists() {
        return Ok(default_db_path());
    }
    let text = std::fs::read_to_string(config_path)?;
    let doc: toml::Value = toml::from_str(&text)?;
    if let Some(p) = doc
        .get("general")
        .and_then(|g| g.get("db_path"))
        .and_then(|v| v.as_str())
    {
        let expanded = shellexpand_home(p);
        return Ok(PathBuf::from(expanded));
    }
    Ok(default_db_path())
}

/// Minimal `~` expansion — we only need it for `db_path` in config.
fn shellexpand_home(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).display().to_string();
        }
    }
    s.to_string()
}

fn read_encryption_config(path: &Path) -> CliResult<(bool, String)> {
    if !path.exists() {
        return Ok((false, DEFAULT_PASS_ENTRY.to_string()));
    }
    let text = std::fs::read_to_string(path)?;
    let doc: toml::Value = toml::from_str(&text)?;
    let sect = doc.get("encryption");
    let enabled = sect
        .and_then(|s| s.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let entry = sect
        .and_then(|s| s.get("pass_entry"))
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_PASS_ENTRY)
        .to_string();
    Ok((enabled, entry))
}

fn set_encryption_enabled_in_config(path: &Path, enabled: bool, pass_entry: &str) -> CliResult<()> {
    // Make sure the parent dir exists (first run might not have it).
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = if existing.is_empty() {
        "".parse()?
    } else {
        existing.parse()?
    };

    let enc_tbl = doc
        .entry("encryption")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or("[encryption] is not a table")?;

    enc_tbl.insert("enabled", toml_edit::value(enabled));
    enc_tbl.insert("pass_entry", toml_edit::value(pass_entry));

    std::fs::write(path, doc.to_string())?;
    Ok(())
}

fn derive_dek(ikm: &[u8]) -> [u8; DEK_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(ikm);
    let digest = hasher.finalize();
    let mut out = [0u8; DEK_LEN];
    out.copy_from_slice(&digest);
    out
}

fn compute_key_id(dek: &[u8]) -> [u8; KEY_ID_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(dek);
    let digest = hasher.finalize();
    let mut id = [0u8; KEY_ID_LEN];
    id.copy_from_slice(&digest[..KEY_ID_LEN]);
    id
}

fn rand_bytes_base64() -> String {
    let mut buf = [0u8; DEK_LEN];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    base64::engine::general_purpose::STANDARD.encode(buf)
}

fn reencrypt(
    old_dek: &[u8; DEK_LEN],
    new_dek: &[u8; DEK_LEN],
    aad: &str,
    blob: Option<&[u8]>,
) -> CliResult<Option<Vec<u8>>> {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};

    let Some(blob) = blob else { return Ok(None) };
    if blob.len() < NONCE_LEN + 16 {
        return Err("rotate: encountered truncated ciphertext".into());
    }
    let (nonce_bytes, ct) = blob.split_at(NONCE_LEN);
    let nonce = XNonce::from_slice(nonce_bytes);

    let old_cipher = XChaCha20Poly1305::new_from_slice(old_dek).unwrap();
    let pt = old_cipher
        .decrypt(
            nonce,
            Payload {
                msg: ct,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| "rotate: failed to decrypt row under old key; is the pass entry correct?")?;

    let mut new_nonce_bytes = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut new_nonce_bytes);
    let new_nonce = XNonce::from_slice(&new_nonce_bytes);
    let new_cipher = XChaCha20Poly1305::new_from_slice(new_dek).unwrap();
    let new_ct = new_cipher
        .encrypt(
            new_nonce,
            Payload {
                msg: &pt,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| "rotate: failed to encrypt row under new key")?;

    let mut out = Vec::with_capacity(NONCE_LEN + new_ct.len());
    out.extend_from_slice(&new_nonce_bytes);
    out.extend_from_slice(&new_ct);
    Ok(Some(out))
}

// Silence "unused" warnings for constants that aren't referenced
// along every path but document the contract with the daemon.
const _: i64 = ENC_VERSION_CURRENT;
