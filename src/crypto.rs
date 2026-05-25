use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore;
use zeroize::Zeroizing;

use crate::error::{Error, Result};

const MAGIC: &[u8; 8] = b"BDPENC1\0";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
const HEADER_LEN: usize = MAGIC.len() + SALT_LEN + NONCE_LEN;

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
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&salt);
    output.extend_from_slice(&nonce);
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

pub fn decrypt_bytes(ciphertext: &[u8], passphrase: &str) -> Result<Vec<u8>> {
    if ciphertext.len() < HEADER_LEN || &ciphertext[..MAGIC.len()] != MAGIC {
        return Err(Error::Crypto(
            "unsupported encrypted file format".to_string(),
        ));
    }

    let salt_start = MAGIC.len();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypts_and_decrypts_roundtrip() {
        let plaintext = b"hello baidupan";
        let encrypted = encrypt_bytes(plaintext, "passphrase").expect("encrypt");

        assert_ne!(encrypted, plaintext);
        assert_eq!(
            decrypt_bytes(&encrypted, "passphrase").expect("decrypt"),
            plaintext
        );
    }

    #[test]
    fn rejects_wrong_passphrase() {
        let encrypted = encrypt_bytes(b"secret", "right").expect("encrypt");
        let error = decrypt_bytes(&encrypted, "wrong").expect_err("wrong passphrase fails");

        assert!(error.to_string().contains("invalid passphrase"));
    }
}
