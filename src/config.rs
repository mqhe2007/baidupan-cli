use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::error::{Error, IoContext, Result};

pub const APP_KEY_ENV: &str = "BAIDUPAN_APP_KEY";
pub const APP_SECRET_ENV: &str = "BAIDUPAN_APP_SECRET";
pub const APP_NAME_ENV: &str = "BAIDUPAN_APP_NAME";
pub const CRYPTO_PASSPHRASE_ENV: &str = "BAIDUPAN_CRYPTO_PASSPHRASE";
pub const USER_AGENT: &str = "pan.baidu.com";

#[derive(Debug, Clone)]
pub struct AppCredentials {
    pub app_key: String,
    pub app_secret: String,
    pub app_name: String,
}

impl AppCredentials {
    pub fn from_env() -> Result<Self> {
        let app_key = env::var(APP_KEY_ENV).map_err(|_| Error::MissingEnv(APP_KEY_ENV))?;
        let app_secret = env::var(APP_SECRET_ENV).map_err(|_| Error::MissingEnv(APP_SECRET_ENV))?;
        let app_name = env::var(APP_NAME_ENV).map_err(|_| Error::MissingEnv(APP_NAME_ENV))?;
        let app_name = app_name.trim();
        if app_name.is_empty() {
            return Err(Error::MissingEnv(APP_NAME_ENV));
        }
        if app_name.contains('/') {
            return Err(Error::InvalidConfig(format!(
                "{APP_NAME_ENV} must be the application name only, without path separators"
            )));
        }

        Ok(Self {
            app_key,
            app_secret,
            app_name: app_name.to_string(),
        })
    }

    pub fn masked_app_key(&self) -> String {
        mask_secret(&self.app_key)
    }

    pub fn app_root(&self) -> String {
        format!("/apps/{}", self.app_name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredToken {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub scope: Option<String>,
}

impl StoredToken {
    pub fn is_expired(&self, now: i64) -> bool {
        self.expires_at <= now + 60
    }
}

#[derive(Debug, Clone)]
pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    pub fn for_current_user() -> Result<Self> {
        let project_dirs = ProjectDirs::from("dev", "baidupan-cli", "baidupan-cli")
            .ok_or(Error::ConfigDirUnavailable)?;
        Ok(Self::new(project_dirs.config_dir().join("tokens.json")))
    }

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn load(&self) -> Result<StoredToken> {
        if !self.path.exists() {
            return Err(Error::NotLoggedIn);
        }

        let json = fs::read_to_string(&self.path).at(&self.path)?;
        Ok(serde_json::from_str(&json)?)
    }

    pub fn save(&self, token: &StoredToken) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).at(parent)?;
        }

        let temp_path = self.path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(token)?;
        fs::write(&temp_path, json).at(&temp_path)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600)).at(&temp_path)?;
        }

        fs::rename(&temp_path, &self.path).at(&self.path)?;
        Ok(())
    }

    pub fn remove(&self) -> Result<()> {
        if self.path.exists() {
            fs::remove_file(&self.path).at(&self.path)?;
        }
        Ok(())
    }
}

pub fn current_unix_timestamp() -> Result<i64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::Time(error.to_string()))?;
    Ok(now.as_secs() as i64)
}

fn mask_secret(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 8 {
        return "****".to_string();
    }

    let head: String = chars.iter().take(4).collect();
    let tail: String = chars
        .iter()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}...{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_short_values() {
        assert_eq!(mask_secret("abc"), "****");
    }

    #[test]
    fn masks_long_values() {
        assert_eq!(mask_secret("abcdefghijkl"), "abcd...ijkl");
    }

    #[test]
    fn stores_and_loads_token() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = TokenStore::new(temp_dir.path().join("tokens.json"));
        let token = StoredToken {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: 42,
            scope: Some("basic netdisk".to_string()),
        };

        store.save(&token).expect("save token");
        assert_eq!(store.load().expect("load token"), token);
    }

    #[test]
    fn detects_expiry_with_buffer() {
        let token = StoredToken {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: 100,
            scope: None,
        };

        assert!(token.is_expired(45));
        assert!(!token.is_expired(1));
    }

    #[test]
    fn computes_app_root() {
        let credentials = AppCredentials {
            app_key: "key".to_string(),
            app_secret: "secret".to_string(),
            app_name: "demo-app".to_string(),
        };

        assert_eq!(credentials.app_root(), "/apps/demo-app");
    }
}
