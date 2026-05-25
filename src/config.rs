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
pub const AUTH_SERVER_ENV: &str = "BAIDUPAN_AUTH_SERVER";
pub const CRYPTO_PASSPHRASE_ENV: &str = "BAIDUPAN_CRYPTO_PASSPHRASE";
pub const USER_AGENT: &str = "pan.baidu.com";

const BUILTIN_APP_NAME: Option<&str> = option_env!("BAIDUPAN_DEFAULT_APP_NAME");
const BUILTIN_AUTH_SERVER: Option<&str> = option_env!("BAIDUPAN_DEFAULT_AUTH_SERVER");
const BUILTIN_CRYPTO_PASSPHRASE: Option<&str> = option_env!("BAIDUPAN_DEFAULT_CRYPTO_PASSPHRASE");

#[derive(Debug, Clone)]
pub struct AppCredentials {
    pub app_key: Option<String>,
    pub app_secret: Option<String>,
    pub app_name: String,
    pub auth_server: Option<String>,
}

impl AppCredentials {
    pub fn from_env() -> Result<Self> {
        Self::from_env_with_mode(true)
    }

    pub fn from_direct_env() -> Result<Self> {
        Self::from_env_with_mode(false)
    }

    fn from_env_with_mode(allow_auth_server: bool) -> Result<Self> {
        let app_key = env::var(APP_KEY_ENV).ok().and_then(normalize_env_value);
        let app_secret = env::var(APP_SECRET_ENV).ok().and_then(normalize_env_value);
        let app_name = read_config_value(APP_NAME_ENV, BUILTIN_APP_NAME)
            .ok_or(Error::MissingEnv(APP_NAME_ENV))?;
        let auth_server = if allow_auth_server {
            read_config_value(AUTH_SERVER_ENV, BUILTIN_AUTH_SERVER)
                .map(|url| url.trim_end_matches('/').to_string())
        } else {
            None
        };
        let app_name = app_name.trim();
        if app_name.is_empty() {
            return Err(Error::MissingEnv(APP_NAME_ENV));
        }
        if app_name.contains('/') {
            return Err(Error::InvalidConfig(format!(
                "{APP_NAME_ENV} must be the application name only, without path separators"
            )));
        }
        if auth_server.is_none() && app_key.is_none() {
            return Err(Error::MissingEnv(APP_KEY_ENV));
        }
        if auth_server.is_none() && app_secret.is_none() {
            return Err(Error::MissingEnv(APP_SECRET_ENV));
        }

        Ok(Self {
            app_key,
            app_secret,
            app_name: app_name.to_string(),
            auth_server,
        })
    }

    pub fn masked_app_key(&self) -> Option<String> {
        self.app_key.as_deref().map(mask_secret)
    }

    pub fn app_root(&self) -> String {
        format!("/apps/{}", self.app_name)
    }

    pub fn auth_server_url(&self) -> Option<&str> {
        self.auth_server.as_deref()
    }

    pub fn oauth_client_id(&self) -> Result<&str> {
        self.app_key
            .as_deref()
            .ok_or(Error::MissingEnv(APP_KEY_ENV))
    }

    pub fn oauth_client_secret(&self) -> Result<&str> {
        self.app_secret
            .as_deref()
            .ok_or(Error::MissingEnv(APP_SECRET_ENV))
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

pub fn configured_crypto_passphrase() -> Option<String> {
    read_config_value(CRYPTO_PASSPHRASE_ENV, BUILTIN_CRYPTO_PASSPHRASE)
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

fn normalize_env_value(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn read_config_value(env_name: &str, builtin: Option<&str>) -> Option<String> {
    env::var(env_name)
        .ok()
        .and_then(normalize_env_value)
        .or_else(|| builtin.and_then(|value| normalize_env_value(value.to_string())))
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::*;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

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
            app_key: Some("key".to_string()),
            app_secret: Some("secret".to_string()),
            app_name: "demo-app".to_string(),
            auth_server: None,
        };

        assert_eq!(credentials.app_root(), "/apps/demo-app");
    }

    #[test]
    fn runtime_value_overrides_builtin_default() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::set_var(APP_NAME_ENV, "runtime-app");

        let configured = read_config_value(APP_NAME_ENV, Some("built-in-app"));

        assert_eq!(configured.as_deref(), Some("runtime-app"));
        std::env::remove_var(APP_NAME_ENV);
    }

    #[test]
    fn builtin_default_is_used_when_runtime_is_missing() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::remove_var(APP_NAME_ENV);

        let configured = read_config_value(APP_NAME_ENV, Some("built-in-app"));

        assert_eq!(configured.as_deref(), Some("built-in-app"));
    }

    #[test]
    fn masks_optional_app_key() {
        let credentials = AppCredentials {
            app_key: None,
            app_secret: None,
            app_name: "demo-app".to_string(),
            auth_server: Some("https://auth.example.com".to_string()),
        };

        assert_eq!(credentials.masked_app_key(), None);
    }

    #[test]
    fn auth_server_mode_does_not_require_local_oauth_credentials() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::set_var(APP_NAME_ENV, "demo-app");
        std::env::set_var(AUTH_SERVER_ENV, "https://auth.example.com/");
        std::env::remove_var(APP_KEY_ENV);
        std::env::remove_var(APP_SECRET_ENV);

        let credentials = AppCredentials::from_env().expect("auth server mode");

        assert_eq!(
            credentials.auth_server_url(),
            Some("https://auth.example.com")
        );
        assert_eq!(
            credentials
                .oauth_client_id()
                .expect_err("no local app key")
                .to_string(),
            format!("missing environment variable {APP_KEY_ENV}")
        );

        std::env::remove_var(APP_NAME_ENV);
        std::env::remove_var(AUTH_SERVER_ENV);
    }
}
