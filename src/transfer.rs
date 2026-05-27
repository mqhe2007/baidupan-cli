use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use directories::ProjectDirs;
use md5::Digest;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::config::configured_crypto_passphrase;
use crate::crypto::{decrypt_bytes, decrypt_file_streaming, encrypt_file_streaming};
use crate::error::{IoContext, Result};

pub const UPLOAD_PART_SIZE: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct PreparedUpload {
    pub source: PathBuf,
    pub materialized: PathBuf,
    pub size: u64,
    pub encrypted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UploadResumeState {
    pub session_key: String,
    pub source_path: PathBuf,
    pub remote_path: String,
    pub materialized_path: PathBuf,
    pub size: u64,
    pub encrypted: bool,
    pub block_list: Vec<String>,
    pub uploadid: String,
}

impl UploadResumeState {
    pub fn is_compatible(
        &self,
        source_path: &Path,
        remote_path: &str,
        materialized_path: &Path,
        size: u64,
        encrypted: bool,
        block_list: &[String],
    ) -> bool {
        self.source_path == source_path
            && self.remote_path == remote_path
            && self.materialized_path == materialized_path
            && self.size == size
            && self.encrypted == encrypted
            && self.block_list == block_list
            && !self.uploadid.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct UploadStateStore {
    root: PathBuf,
}

impl UploadStateStore {
    pub fn for_current_user() -> Result<Self> {
        let project_dirs = ProjectDirs::from("dev", "baidupan-cli", "baidupan-cli")
            .ok_or(crate::Error::ConfigDirUnavailable)?;
        Ok(Self::new(project_dirs.config_dir().join("uploads")))
    }

    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn session_key(&self, source: &Path, remote: &str, encrypt: bool) -> Result<String> {
        let canonical = source.canonicalize().at(source)?;
        let metadata = fs::metadata(&canonical).at(&canonical)?;
        let modified = metadata
            .modified()
            .map_err(|error| crate::Error::Crypto(error.to_string()))?
            .duration_since(UNIX_EPOCH)
            .map_err(|error| crate::Error::Crypto(error.to_string()))?
            .as_secs();

        Ok(format!(
            "{:x}",
            md5::compute(format!(
                "{}|{}|{}|{}|{}",
                canonical.display(),
                remote,
                metadata.len(),
                modified,
                encrypt
            ))
        ))
    }

    pub fn cache_path(&self, session_key: &str) -> PathBuf {
        self.root.join("cache").join(format!("{session_key}.bin"))
    }

    pub fn load(&self, session_key: &str) -> Result<Option<UploadResumeState>> {
        let path = self.state_path(session_key);
        if !path.exists() {
            return Ok(None);
        }

        let json = fs::read_to_string(&path).at(&path)?;
        Ok(Some(serde_json::from_str(&json)?))
    }

    pub fn save(&self, state: &UploadResumeState) -> Result<()> {
        let path = self.state_path(&state.session_key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).at(parent)?;
        }

        let temp_path = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(state)?;
        fs::write(&temp_path, json).at(&temp_path)?;
        fs::rename(&temp_path, &path).at(&path)?;
        Ok(())
    }

    pub fn remove(&self, session_key: &str) -> Result<()> {
        let path = self.state_path(session_key);
        if path.exists() {
            fs::remove_file(&path).at(&path)?;
        }
        Ok(())
    }

    pub fn cleanup_success(
        &self,
        session_key: &str,
        materialized_path: &Path,
        encrypted: bool,
    ) -> Result<()> {
        self.remove(session_key)?;
        if encrypted && materialized_path.exists() {
            fs::remove_file(materialized_path).at(materialized_path)?;
        }
        Ok(())
    }

    fn state_path(&self, session_key: &str) -> PathBuf {
        self.root
            .join("sessions")
            .join(format!("{session_key}.json"))
    }
}

#[derive(Debug, Clone)]
pub struct PreparedDownload {
    pub remote_path: String,
    pub destination: PathBuf,
    pub temp_path: PathBuf,
    pub state_path: PathBuf,
    pub resume_from: u64,
    pub decrypt: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct DownloadResumeState {
    remote_path: String,
    decrypt: bool,
}

#[derive(Debug, Clone)]
pub struct TransferPlanner;

impl TransferPlanner {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }

    pub fn prepare_upload(&self, local: &Path, encrypt: bool) -> Result<PreparedUpload> {
        self.prepare_upload_with_cache(local, encrypt, None)
    }

    pub fn prepare_upload_with_cache(
        &self,
        local: &Path,
        encrypt: bool,
        cache_path: Option<&Path>,
    ) -> Result<PreparedUpload> {
        let local = local.to_path_buf();

        if encrypt {
            if let Some(cache_path) = cache_path {
                if cache_path.exists() {
                    let size = fs::metadata(cache_path).at(cache_path)?.len();
                    return Ok(PreparedUpload {
                        source: local,
                        materialized: cache_path.to_path_buf(),
                        size,
                        encrypted: true,
                    });
                }

                if let Some(parent) = cache_path.parent() {
                    fs::create_dir_all(parent).at(parent)?;
                }

                let passphrase = read_passphrase()?;
                encrypt_file_streaming(&local, cache_path, &passphrase)
                    .map_err(|error| crate::Error::Crypto(error.to_string()))?;
                let size = fs::metadata(cache_path).at(cache_path)?.len();
                return Ok(PreparedUpload {
                    source: local,
                    materialized: cache_path.to_path_buf(),
                    size,
                    encrypted: true,
                });
            }

            let passphrase = read_passphrase()?;
            let temp =
                NamedTempFile::new().map_err(|error| crate::Error::Crypto(error.to_string()))?;
            let temp_path = temp.path().to_path_buf();
            encrypt_file_streaming(&local, &temp_path, &passphrase)
                .map_err(|error| crate::Error::Crypto(error.to_string()))?;
            let (_file, persisted) = temp
                .keep()
                .map_err(|error| crate::Error::Crypto(error.error.to_string()))?;
            let size = fs::metadata(&persisted).at(&persisted)?.len();
            return Ok(PreparedUpload {
                source: local,
                materialized: persisted,
                size,
                encrypted: true,
            });
        }

        let size = fs::metadata(&local).at(&local)?.len();
        Ok(PreparedUpload {
            source: local.clone(),
            materialized: local,
            size,
            encrypted: false,
        })
    }

    pub fn assert_download_target(&self, local: &Path, force: bool) -> Result<()> {
        if local.exists() && !force {
            return Err(crate::Error::Api(format!(
                "destination {} exists; rerun with --force",
                local.display()
            )));
        }
        Ok(())
    }

    pub fn prepare_download(
        &self,
        remote_path: &str,
        destination: &Path,
        decrypt: bool,
        force: bool,
    ) -> Result<PreparedDownload> {
        self.assert_download_target(destination, force)?;

        let temp_path = sidecar_path(destination, "baidupan.part");
        let state_path = sidecar_path(destination, "baidupan.resume.json");
        let mut resume_from = 0;

        let has_temp = temp_path.exists();
        let has_state = state_path.exists();

        if has_temp && has_state {
            let state: DownloadResumeState =
                serde_json::from_str(&fs::read_to_string(&state_path).at(&state_path)?)?;
            if state.remote_path == remote_path && state.decrypt == decrypt {
                resume_from = fs::metadata(&temp_path).at(&temp_path)?.len();
            } else {
                cleanup_if_exists(&temp_path)?;
                cleanup_if_exists(&state_path)?;
            }
        } else if has_temp || has_state {
            cleanup_if_exists(&temp_path)?;
            cleanup_if_exists(&state_path)?;
        }

        let state = DownloadResumeState {
            remote_path: remote_path.to_string(),
            decrypt,
        };
        fs::write(&state_path, serde_json::to_vec_pretty(&state)?).at(&state_path)?;

        Ok(PreparedDownload {
            remote_path: remote_path.to_string(),
            destination: destination.to_path_buf(),
            temp_path,
            state_path,
            resume_from,
            decrypt,
        })
    }

    /// Decrypt a downloaded file.  Auto-detects V1 (whole-file) vs V2
    /// (chunked / streaming) by inspecting the leading MAGIC bytes.
    pub fn decrypt_downloaded_file(&self, source: &Path, destination: &Path) -> Result<()> {
        let passphrase = read_passphrase()?;

        // Peek at the magic bytes to route to the right format.
        let mut file = fs::File::open(source).at(source)?;
        let mut magic = [0_u8; 8];
        let is_v2 = file.read_exact(&mut magic).is_ok() && &magic == b"BDPENC2\0";
        drop(file);

        if is_v2 {
            return decrypt_file_streaming(source, destination, &passphrase)
                .map_err(|error| crate::Error::Crypto(error.to_string()));
        }

        // V1 fallback (existing encrypted files)
        let payload = fs::read(source).at(source)?;
        let plaintext = decrypt_bytes(&payload, &passphrase)?;
        fs::write(destination, plaintext).at(destination)?;
        Ok(())
    }

    /// Compute the MD5 block-list for upload precreate, reading the file
    /// in UPLOAD_PART_SIZE chunks so that large files stay bounded.
    pub fn block_list(&self, source: &Path) -> Result<Vec<String>> {
        let file = fs::File::open(source).at(source)?;
        let mut reader = BufReader::with_capacity(UPLOAD_PART_SIZE, file);
        let mut buf = vec![0_u8; UPLOAD_PART_SIZE];
        let mut block_list = Vec::new();

        loop {
            let n = reader.read(&mut buf).at(source)?;
            if n == 0 {
                break;
            }
            let digest: Digest = md5::compute(&buf[..n]);
            block_list.push(format!("{:x}", digest));
        }

        if block_list.is_empty() {
            let digest: Digest = md5::compute([]);
            block_list.push(format!("{:x}", digest));
        }

        Ok(block_list)
    }

    pub fn finalize_download(&self, prepared: &PreparedDownload, force: bool) -> Result<()> {
        if prepared.decrypt {
            self.decrypt_downloaded_file(&prepared.temp_path, &prepared.destination)?;
            cleanup_if_exists(&prepared.temp_path)?;
        } else {
            if force && prepared.destination.exists() {
                cleanup_if_exists(&prepared.destination)?;
            }
            fs::rename(&prepared.temp_path, &prepared.destination).at(&prepared.destination)?;
        }

        cleanup_if_exists(&prepared.state_path)?;
        Ok(())
    }
}

fn sidecar_path(destination: &Path, suffix: &str) -> PathBuf {
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("download");

    parent.join(format!(".{file_name}.{suffix}"))
}

fn cleanup_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path).at(path)?;
    }
    Ok(())
}

fn read_passphrase() -> Result<String> {
    configured_crypto_passphrase().ok_or_else(|| {
        crate::Error::Crypto(format!(
            "encryption/decryption requires {} environment variable",
            crate::config::CRYPTO_PASSPHRASE_ENV
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::*;
    use crate::config::CRYPTO_PASSPHRASE_ENV;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn prepares_encrypted_upload() {
        let _guard = env_lock().lock().expect("env lock");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let source = temp_dir.path().join("note.txt");
        fs::write(&source, b"hello").expect("source");
        std::env::set_var(CRYPTO_PASSPHRASE_ENV, "test-pass");

        let prepared = TransferPlanner::new()
            .expect("planner")
            .prepare_upload(&source, true)
            .expect("prepare");

        assert!(prepared.encrypted);
        assert_ne!(prepared.materialized, source);
        assert!(prepared.size > 0);

        std::env::remove_var(CRYPTO_PASSPHRASE_ENV);
    }

    #[test]
    fn decrypts_downloaded_file() {
        let _guard = env_lock().lock().expect("env lock");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let encrypted = temp_dir.path().join("payload.bin");
        let output = temp_dir.path().join("plain.txt");
        std::env::set_var(CRYPTO_PASSPHRASE_ENV, "test-pass");

        fs::write(
            &encrypted,
            crate::crypto::encrypt_bytes(b"world", "test-pass").expect("encrypt bytes"),
        )
        .expect("write encrypted");

        TransferPlanner::new()
            .expect("planner")
            .decrypt_downloaded_file(&encrypted, &output)
            .expect("decrypt");

        assert_eq!(fs::read(&output).expect("read output"), b"world");
        std::env::remove_var(CRYPTO_PASSPHRASE_ENV);
    }

    #[test]
    fn decrypts_downloaded_file_v2() {
        let _guard = env_lock().lock().expect("env lock");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let source = temp_dir.path().join("src.bin");
        let encrypted = temp_dir.path().join("payload.bin");
        let output = temp_dir.path().join("plain.txt");
        std::env::set_var(CRYPTO_PASSPHRASE_ENV, "test-pass");

        fs::write(&source, b"streaming world").expect("write source");
        encrypt_file_streaming(&source, &encrypted, "test-pass").expect("encrypt v2");

        TransferPlanner::new()
            .expect("planner")
            .decrypt_downloaded_file(&encrypted, &output)
            .expect("decrypt v2");

        assert_eq!(fs::read(&output).expect("read output"), b"streaming world");
        std::env::remove_var(CRYPTO_PASSPHRASE_ENV);
    }

    #[test]
    fn computes_md5_block_list() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let source = temp_dir.path().join("data.bin");
        fs::write(&source, b"abc").expect("write source");

        let blocks = TransferPlanner::new()
            .expect("planner")
            .block_list(&source)
            .expect("block list");

        assert_eq!(blocks, vec!["900150983cd24fb0d6963f7d28e17f72"]);
    }

    #[test]
    fn block_list_empty_file() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let source = temp_dir.path().join("empty.bin");
        fs::write(&source, b"").expect("write empty");

        let blocks = TransferPlanner::new()
            .expect("planner")
            .block_list(&source)
            .expect("block list");

        // 0-byte file: one block with MD5 of empty input
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0], format!("{:x}", md5::compute([])));
    }

    #[test]
    fn stores_and_loads_upload_resume_state() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = UploadStateStore::new(temp_dir.path().join("uploads"));
        let state = UploadResumeState {
            session_key: "session".to_string(),
            source_path: PathBuf::from("/tmp/source.txt"),
            remote_path: "/apps/demo/source.txt".to_string(),
            materialized_path: PathBuf::from("/tmp/cache.bin"),
            size: 42,
            encrypted: true,
            block_list: vec!["abc".to_string()],
            uploadid: "uploadid".to_string(),
        };

        store.save(&state).expect("save state");
        assert_eq!(store.load("session").expect("load state"), Some(state));
    }

    #[test]
    fn reuses_cached_encrypted_upload() {
        let _guard = env_lock().lock().expect("env lock");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let source = temp_dir.path().join("note.txt");
        let cached = temp_dir.path().join("cache").join("payload.bin");
        fs::write(&source, b"hello").expect("source");
        std::env::set_var(CRYPTO_PASSPHRASE_ENV, "test-pass");

        let planner = TransferPlanner::new().expect("planner");
        let first = planner
            .prepare_upload_with_cache(&source, true, Some(&cached))
            .expect("first prepare");
        let second = planner
            .prepare_upload_with_cache(&source, true, Some(&cached))
            .expect("second prepare");

        assert_eq!(first.materialized, cached);
        assert_eq!(second.materialized, cached);
        assert_eq!(first.size, second.size);

        std::env::remove_var(CRYPTO_PASSPHRASE_ENV);
    }

    #[test]
    fn prepares_download_resume_when_sidecar_matches() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let destination = temp_dir.path().join("movie.bin");
        let temp_path = sidecar_path(&destination, "baidupan.part");
        let state_path = sidecar_path(&destination, "baidupan.resume.json");

        fs::write(&temp_path, b"partial").expect("partial file");
        fs::write(
            &state_path,
            serde_json::to_vec(&DownloadResumeState {
                remote_path: "/apps/movie.bin".to_string(),
                decrypt: false,
            })
            .expect("state"),
        )
        .expect("state write");

        let prepared = TransferPlanner::new()
            .expect("planner")
            .prepare_download("/apps/movie.bin", &destination, false, false)
            .expect("prepare download");

        assert_eq!(prepared.resume_from, 7);
        assert_eq!(prepared.temp_path, temp_path);
    }

    #[test]
    fn discards_stale_download_resume_state() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let destination = temp_dir.path().join("movie.bin");
        let temp_path = sidecar_path(&destination, "baidupan.part");
        let state_path = sidecar_path(&destination, "baidupan.resume.json");

        fs::write(&temp_path, b"partial").expect("partial file");
        fs::write(
            &state_path,
            serde_json::to_vec(&DownloadResumeState {
                remote_path: "/apps/other.bin".to_string(),
                decrypt: false,
            })
            .expect("state"),
        )
        .expect("state write");

        let prepared = TransferPlanner::new()
            .expect("planner")
            .prepare_download("/apps/movie.bin", &destination, false, false)
            .expect("prepare download");

        assert_eq!(prepared.resume_from, 0);
        assert!(!prepared.temp_path.exists());
        assert!(prepared.state_path.exists());
    }

    #[test]
    fn finalizes_plain_download_with_force() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let destination = temp_dir.path().join("movie.bin");
        let temp_path = sidecar_path(&destination, "baidupan.part");
        let state_path = sidecar_path(&destination, "baidupan.resume.json");
        fs::write(&destination, b"old").expect("old dest");
        fs::write(&temp_path, b"new").expect("temp payload");
        fs::write(
            &state_path,
            serde_json::to_vec(&DownloadResumeState {
                remote_path: "/apps/movie.bin".to_string(),
                decrypt: false,
            })
            .expect("state"),
        )
        .expect("state write");

        let prepared = PreparedDownload {
            remote_path: "/apps/movie.bin".to_string(),
            destination: destination.clone(),
            temp_path: temp_path.clone(),
            state_path: state_path.clone(),
            resume_from: 3,
            decrypt: false,
        };

        TransferPlanner::new()
            .expect("planner")
            .finalize_download(&prepared, true)
            .expect("finalize");

        assert_eq!(fs::read(&destination).expect("final payload"), b"new");
        assert!(!temp_path.exists());
        assert!(!state_path.exists());
    }
}
