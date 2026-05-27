use std::fs;
use std::io::{BufReader, Read, Write};
use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore;
use zeroize::Zeroizing;

use crate::error::{Error, Result};

const MAGIC_V1: &[u8; 8] = b"BDPENC1\0";
const MAGIC_V2: &[u8; 8] = b"BDPENC2\0";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
const HEADER_LEN: usize = MAGIC_V1.len() + SALT_LEN + NONCE_LEN;
#[allow(dead_code)]
const HEADER_V2_LEN: usize = MAGIC_V2.len() + SALT_LEN + 8; // 8 reserved
const CHUNK_SIZE: usize = 64 * 1024 * 1024; // 64 MiB
#[allow(dead_code)]
const CHUNK_META_LEN: usize = NONCE_LEN + 4; // nonce + u32 length

// ── V1 (whole-file AES-256-GCM) ────────────────────────────────────────────
// Kept for backward-compatible decryption of existing encrypted files.

pub fn encrypt_bytes(plaintext: &[u8], passphrase: &str) -> Result<Vec<u8>> {
    let mut salt = [0_u8; SALT_LEN];
    let mut nonce = [0_u8; NONCE_LEN];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce);

    let key = derive_key(passphrase, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(key.as_slice())
        .map_err(|error| Error::Crypto(error.to_string()))?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|error| Error::Crypto(error.to_string()))?;

    let mut output = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    output.extend_from_slice(MAGIC_V1);
    output.extend_from_slice(&salt);
    output.extend_from_slice(&nonce);
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

pub fn decrypt_bytes(ciphertext: &[u8], passphrase: &str) -> Result<Vec<u8>> {
    if ciphertext.len() < HEADER_LEN || &ciphertext[..MAGIC_V1.len()] != MAGIC_V1 {
        return Err(Error::Crypto(
            "unsupported encrypted file format".to_string(),
        ));
    }

    let salt_start = MAGIC_V1.len();
    let nonce_start = salt_start + SALT_LEN;
    let payload_start = nonce_start + NONCE_LEN;

    let salt = &ciphertext[salt_start..nonce_start];
    let nonce = &ciphertext[nonce_start..payload_start];
    let payload = &ciphertext[payload_start..];

    let key = derive_key(passphrase, salt)?;
    let cipher = Aes256Gcm::new_from_slice(key.as_slice())
        .map_err(|error| Error::Crypto(error.to_string()))?;

    cipher
        .decrypt(Nonce::from_slice(nonce), payload)
        .map_err(|_| Error::Crypto("invalid passphrase or corrupted ciphertext".to_string()))
}

// ── V2: chunked streaming AES-256-GCM ───────────────────────────────────────
//
// File layout:
//   MAGIC_V2 (8 bytes) | SALT (16) | RESERVED (8) | CHUNK* ...
//   Each chunk: NONCE (12) | PAYLOAD_LEN u32 LE (4) | CIPHERTEXT+TAG
//
// Every chunk gets its own random nonce; the key is derived once from
// passphrase + salt via Argon2id.  This keeps memory bounded at
// CHUNK_SIZE + constant overhead and works on arbitrary-size files.

pub fn encrypt_file_streaming(
    source_path: &Path,
    dest_path: &Path,
    passphrase: &str,
) -> Result<()> {
    let mut salt = [0_u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let key = derive_key(passphrase, &salt)?;

    let source = fs::File::open(source_path)
        .map_err(|e| Error::Crypto(format!("cannot open {}: {e}", source_path.display())))?;
    let mut reader = BufReader::with_capacity(CHUNK_SIZE, source);

    let mut dest = fs::File::create(dest_path)
        .map_err(|e| Error::Crypto(format!("cannot create {}: {e}", dest_path.display())))?;

    // V2 header
    dest.write_all(MAGIC_V2)
        .map_err(|e| Error::Crypto(e.to_string()))?;
    dest.write_all(&salt)
        .map_err(|e| Error::Crypto(e.to_string()))?;
    dest.write_all(&[0_u8; 8])
        .map_err(|e| Error::Crypto(e.to_string()))?;

    let mut buf = vec![0_u8; CHUNK_SIZE];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| Error::Crypto(format!("failed to read source chunk: {e}")))?;
        if n == 0 {
            break;
        }

        let mut nonce = [0_u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);

        let cipher =
            Aes256Gcm::new_from_slice(key.as_slice()).map_err(|e| Error::Crypto(e.to_string()))?;
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), &buf[..n])
            .map_err(|e| Error::Crypto(e.to_string()))?;

        let payload_len = ciphertext.len() as u32;
        dest.write_all(&nonce)
            .map_err(|e| Error::Crypto(e.to_string()))?;
        dest.write_all(&payload_len.to_le_bytes())
            .map_err(|e| Error::Crypto(e.to_string()))?;
        dest.write_all(&ciphertext)
            .map_err(|e| Error::Crypto(e.to_string()))?;
    }

    dest.flush().map_err(|e| Error::Crypto(e.to_string()))?;
    Ok(())
}

/// Decrypt a V2-format file to `dest_path` in streaming fashion.
/// Returns `Ok(())` on success, or an error (including format mismatch).
pub fn decrypt_file_streaming(
    source_path: &Path,
    dest_path: &Path,
    passphrase: &str,
) -> Result<()> {
    let source = fs::File::open(source_path)
        .map_err(|e| Error::Crypto(format!("cannot open {}: {e}", source_path.display())))?;
    let mut reader = BufReader::with_capacity(CHUNK_SIZE, source);

    // Read and validate V2 header
    let mut magic = [0_u8; MAGIC_V2.len()];
    reader
        .read_exact(&mut magic)
        .map_err(|_| Error::Crypto("file too short for V2 encrypted format".to_string()))?;
    if &magic != MAGIC_V2 {
        return Err(Error::Crypto("not a V2 encrypted file".to_string()));
    }

    let mut salt = [0_u8; SALT_LEN];
    reader
        .read_exact(&mut salt)
        .map_err(|e| Error::Crypto(format!("failed to read V2 salt: {e}")))?;

    let mut reserved = [0_u8; 8];
    reader
        .read_exact(&mut reserved)
        .map_err(|e| Error::Crypto(format!("failed to read V2 reserved: {e}")))?;

    let key = derive_key(passphrase, &salt)?;

    let mut dest = fs::File::create(dest_path)
        .map_err(|e| Error::Crypto(format!("cannot create {}: {e}", dest_path.display())))?;

    loop {
        // Read chunk nonce
        let mut nonce = [0_u8; NONCE_LEN];
        match reader.read_exact(&mut nonce) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => {
                return Err(Error::Crypto(format!("failed to read chunk nonce: {e}")));
            }
        }

        // Read chunk payload length
        let mut len_buf = [0_u8; 4];
        reader
            .read_exact(&mut len_buf)
            .map_err(|e| Error::Crypto(format!("failed to read chunk length: {e}")))?;
        let payload_len = u32::from_le_bytes(len_buf) as usize;

        // Read ciphertext + tag
        let mut ciphertext = vec![0_u8; payload_len];
        reader
            .read_exact(&mut ciphertext)
            .map_err(|e| Error::Crypto(format!("failed to read chunk payload: {e}")))?;

        let cipher =
            Aes256Gcm::new_from_slice(key.as_slice()).map_err(|e| Error::Crypto(e.to_string()))?;
        let plaintext = cipher
            .decrypt(Nonce::from_slice(&nonce), &ciphertext[..])
            .map_err(|_| Error::Crypto("invalid passphrase or corrupted ciphertext".to_string()))?;

        dest.write_all(&plaintext)
            .map_err(|e| Error::Crypto(e.to_string()))?;
    }

    dest.flush().map_err(|e| Error::Crypto(e.to_string()))?;
    Ok(())
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn derive_key(passphrase: &str, salt: &[u8]) -> Result<Zeroizing<[u8; KEY_LEN]>> {
    let params = Params::new(19_456, 2, 1, Some(KEY_LEN))
        .map_err(|error| Error::Crypto(error.to_string()))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0_u8; KEY_LEN]);
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|error| Error::Crypto(error.to_string()))?;
    Ok(key)
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypts_and_decrypts_roundtrip_v1() {
        let plaintext = b"hello baidupan";
        let encrypted = encrypt_bytes(plaintext, "passphrase").expect("encrypt");

        assert_ne!(encrypted, plaintext);
        assert_eq!(
            decrypt_bytes(&encrypted, "passphrase").expect("decrypt"),
            plaintext
        );
    }

    #[test]
    fn rejects_wrong_passphrase_v1() {
        let encrypted = encrypt_bytes(b"secret", "right").expect("encrypt");
        let error = decrypt_bytes(&encrypted, "wrong").expect_err("wrong passphrase fails");

        assert!(error.to_string().contains("invalid passphrase"));
    }

    #[test]
    fn v2_streaming_roundtrip_single_chunk() {
        let plaintext = b"hello streaming baidupan v2";
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let source = temp_dir.path().join("plain.bin");
        let encrypted = temp_dir.path().join("enc.bin");
        let decrypted = temp_dir.path().join("dec.bin");

        fs::write(&source, plaintext).expect("write source");

        encrypt_file_streaming(&source, &encrypted, "passphrase").expect("encrypt v2");
        decrypt_file_streaming(&encrypted, &decrypted, "passphrase").expect("decrypt v2");

        assert_eq!(fs::read(&decrypted).expect("read dec"), plaintext);
    }

    #[test]
    fn v2_streaming_roundtrip_multiple_chunks() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let source = temp_dir.path().join("big.bin");
        let encrypted = temp_dir.path().join("big.enc");
        let decrypted = temp_dir.path().join("big.dec");

        // Write ~130 MB (2+ chunks) to exercise the chunk loop
        let size = CHUNK_SIZE + CHUNK_SIZE / 2;
        let mut f = fs::File::create(&source).expect("create big");
        // Use a repeating pattern so we can verify integrity
        let pattern: Vec<u8> = (0u8..=255).cycle().take(size).collect();
        f.write_all(&pattern).expect("write big");
        f.flush().expect("flush big");

        encrypt_file_streaming(&source, &encrypted, "passphrase").expect("encrypt v2 big");
        decrypt_file_streaming(&encrypted, &decrypted, "passphrase").expect("decrypt v2 big");

        let dec = fs::read(&decrypted).expect("read dec");
        assert_eq!(dec.len(), size);
        assert_eq!(dec, pattern);
    }

    #[test]
    fn v2_streaming_encrypted_file_has_correct_header() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let source = temp_dir.path().join("src.bin");
        let encrypted = temp_dir.path().join("enc.bin");

        fs::write(&source, b"data").expect("write");

        encrypt_file_streaming(&source, &encrypted, "passphrase").expect("encrypt");

        let enc = fs::read(&encrypted).expect("read enc");
        assert_eq!(&enc[..8], MAGIC_V2);
        // salt (16) + reserved (8) + first chunk nonce (12) + len (4)
        let after_header = 8 + 16 + 8;
        assert!(enc.len() > after_header);
    }

    #[test]
    fn v2_rejects_wrong_passphrase() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let source = temp_dir.path().join("src.bin");
        let encrypted = temp_dir.path().join("enc.bin");
        let decrypted = temp_dir.path().join("dec.bin");

        fs::write(&source, b"secret").expect("write");
        encrypt_file_streaming(&source, &encrypted, "right").expect("encrypt");

        let err = decrypt_file_streaming(&encrypted, &decrypted, "wrong")
            .expect_err("wrong passphrase should fail");
        assert!(err.to_string().contains("invalid passphrase"));
    }

    #[test]
    fn v1_encrypted_file_is_not_accepted_as_v2() {
        let encrypted_v1 = encrypt_bytes(b"hello", "passphrase").expect("encrypt v1");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let enc_path = temp_dir.path().join("enc.bin");
        let dec_path = temp_dir.path().join("dec.bin");
        fs::write(&enc_path, &encrypted_v1).expect("write v1");

        let err = decrypt_file_streaming(&enc_path, &dec_path, "passphrase").expect_err("v1 as v2");
        assert!(err.to_string().contains("not a V2 encrypted file"));
    }
}
