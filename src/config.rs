use std::{
    env, fmt, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use keyring::{Entry, Error as KeyringError};
use serde::{Deserialize, Serialize};

use crate::{ai::normalize_base_api_url, cli::ConfigKey};

const CONFIG_DIR_NAME: &str = "git-buddy";
const CONFIG_FILE_NAME: &str = "config.toml";
const KEYRING_SERVICE: &str = "git-buddy";
const KEYRING_USER: &str = "default";
const ENV_API_TOKEN: &str = "API_TOKEN";
const ENV_BASE_API_URL: &str = "BASE_API_URL";
const ENV_BASE_MODEL: &str = "BASE_MODEL";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    Gemini,
    OpenAiCompatible,
}

impl Provider {
    pub fn default_base_api_url(self) -> &'static str {
        match self {
            Provider::Gemini => "https://generativelanguage.googleapis.com/v1beta/openai",
            Provider::OpenAiCompatible => "https://api.openai.com/v1",
        }
    }

    pub fn default_base_model(self) -> &'static str {
        match self {
            Provider::Gemini => "gemini-2.5-flash",
            Provider::OpenAiCompatible => "gpt-4.1-mini",
        }
    }
}

impl Default for Provider {
    fn default() -> Self {
        Self::Gemini
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Provider::Gemini => write!(f, "gemini"),
            Provider::OpenAiCompatible => write!(f, "openai-compatible"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CommitStyle {
    Standard,
    Conventional,
}

impl Default for CommitStyle {
    fn default() -> Self {
        Self::Standard
    }
}

impl fmt::Display for CommitStyle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommitStyle::Standard => write!(f, "standard"),
            CommitStyle::Conventional => write!(f, "conventional"),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<Provider>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_api_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_style: Option<CommitStyle>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueSource {
    Environment,
    ConfigFile,
    BuiltIn,
    Keychain,
}

impl fmt::Display for ValueSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValueSource::Environment => write!(f, "environment"),
            ValueSource::ConfigFile => write!(f, "config file"),
            ValueSource::BuiltIn => write!(f, "built-in default"),
            ValueSource::Keychain => write!(f, "system keychain"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedValue<T> {
    pub value: T,
    pub source: ValueSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAiSettings {
    pub api_token: ResolvedValue<String>,
    pub provider: ResolvedValue<Provider>,
    pub base_api_url: ResolvedValue<String>,
    pub base_model: ResolvedValue<String>,
    pub commit_style: ResolvedValue<CommitStyle>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedNonSecretSettings {
    pub provider: ResolvedValue<Provider>,
    pub base_api_url: ResolvedValue<String>,
    pub base_model: ResolvedValue<String>,
    pub commit_style: ResolvedValue<CommitStyle>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenStatus {
    EnvironmentOverride,
    Keychain,
    Missing,
}

pub fn config_path() -> Result<PathBuf> {
    let dir = dirs::config_dir().context("unable to determine the user config directory")?;
    Ok(dir.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME))
}

pub fn load_file() -> Result<FileConfig> {
    let path = config_path()?;
    load_file_from_path(&path)
}

pub fn set_config_value(key: ConfigKey, value: &str) -> Result<PathBuf> {
    let path = config_path()?;
    let mut config = load_file_from_path(&path)?;

    match key {
        ConfigKey::Provider => {
            config.provider = Some(parse_provider(value)?);
        }
        ConfigKey::BaseApiUrl => {
            config.base_api_url = Some(normalize_base_api_url(value)?);
        }
        ConfigKey::BaseModel => {
            let value = value.trim();
            if value.is_empty() {
                bail!("BASE_MODEL cannot be empty");
            }
            config.base_model = Some(value.to_string());
        }
        ConfigKey::CommitStyle => {
            config.commit_style = Some(parse_commit_style(value)?);
        }
    }

    save_file_to_path(&path, &config)?;
    Ok(path)
}

pub fn unset_config_value(key: ConfigKey) -> Result<PathBuf> {
    let path = config_path()?;
    let mut config = load_file_from_path(&path)?;

    match key {
        ConfigKey::Provider => config.provider = None,
        ConfigKey::BaseApiUrl => config.base_api_url = None,
        ConfigKey::BaseModel => config.base_model = None,
        ConfigKey::CommitStyle => config.commit_style = None,
    }

    save_file_to_path(&path, &config)?;
    Ok(path)
}

pub fn resolve_ai_settings() -> Result<ResolvedAiSettings> {
    let file = load_file()?;
    let env_api_token = env::var(ENV_API_TOKEN).ok();
    let env_base_api_url = env::var(ENV_BASE_API_URL).ok();
    let env_base_model = env::var(ENV_BASE_MODEL).ok();
    let keychain_token = load_api_token()?;

    resolve_ai_settings_from(
        &file,
        env_api_token.as_deref(),
        env_base_api_url.as_deref(),
        env_base_model.as_deref(),
        keychain_token.as_deref(),
    )
}

pub fn store_api_token(token: &str) -> Result<()> {
    let token = token.trim();
    if token.is_empty() {
        bail!("API token cannot be empty");
    }

    keyring_entry()?
        .set_password(token)
        .context("failed to store API token in the system keychain")?;
    Ok(())
}

pub fn load_api_token() -> Result<Option<String>> {
    match keyring_entry()?
        .get_password()
        .context("failed to read API token from the system keychain")
    {
        Ok(token) => Ok(Some(token)),
        Err(error) => match error.downcast_ref::<KeyringError>() {
            Some(KeyringError::NoEntry) => Ok(None),
            _ => Err(error),
        },
    }
}

pub fn delete_api_token() -> Result<bool> {
    match keyring_entry()?
        .delete_credential()
        .context("failed to delete API token from the system keychain")
    {
        Ok(()) => Ok(true),
        Err(error) => match error.downcast_ref::<KeyringError>() {
            Some(KeyringError::NoEntry) => Ok(false),
            _ => Err(error),
        },
    }
}

pub fn token_status() -> Result<TokenStatus> {
    if env::var_os(ENV_API_TOKEN).is_some() {
        return Ok(TokenStatus::EnvironmentOverride);
    }

    if load_api_token()?.is_some() {
        Ok(TokenStatus::Keychain)
    } else {
        Ok(TokenStatus::Missing)
    }
}

pub fn load_file_from_path(path: &Path) -> Result<FileConfig> {
    if !path.exists() {
        return Ok(FileConfig::default());
    }

    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read config file at {}", path.display()))?;
    let config = toml::from_str(&raw)
        .with_context(|| format!("failed to parse config file at {}", path.display()))?;
    Ok(config)
}

pub fn save_file_to_path(path: &Path, config: &FileConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }

    let raw = toml::to_string_pretty(config).context("failed to serialize config file")?;
    fs::write(path, raw).with_context(|| format!("failed to write config file {}", path.display()))
}

fn parse_provider(raw: &str) -> Result<Provider> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "gemini" => Ok(Provider::Gemini),
        "openai-compatible" | "openai_compatible" | "openaicompatible" => {
            Ok(Provider::OpenAiCompatible)
        }
        _ => bail!("provider must be 'gemini' or 'openai-compatible'"),
    }
}

fn parse_commit_style(raw: &str) -> Result<CommitStyle> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "standard" => Ok(CommitStyle::Standard),
        "conventional" => Ok(CommitStyle::Conventional),
        _ => bail!("commit style must be 'standard' or 'conventional'"),
    }
}

pub fn resolve_ai_settings_from(
    file: &FileConfig,
    env_api_token: Option<&str>,
    env_base_api_url: Option<&str>,
    env_base_model: Option<&str>,
    keychain_token: Option<&str>,
) -> Result<ResolvedAiSettings> {
    let non_secret = resolve_non_secret_settings_from(file, env_base_api_url, env_base_model)?;
    let api_token = if let Some(token) = env_api_token {
        let token = token.trim();
        if token.is_empty() {
            bail!("API_TOKEN cannot be empty");
        }
        ResolvedValue {
            value: token.to_string(),
            source: ValueSource::Environment,
        }
    } else if let Some(token) = keychain_token {
        let token = token.trim();
        if token.is_empty() {
            bail!("stored API token cannot be empty");
        }
        ResolvedValue {
            value: token.to_string(),
            source: ValueSource::Keychain,
        }
    } else {
        bail!(
            "missing API token; run `gitbuddy config` or `gitbuddy auth login`, or set API_TOKEN"
        );
    };

    Ok(ResolvedAiSettings {
        api_token,
        provider: non_secret.provider,
        base_api_url: non_secret.base_api_url,
        base_model: non_secret.base_model,
        commit_style: non_secret.commit_style,
    })
}

pub fn resolve_non_secret_settings() -> Result<ResolvedNonSecretSettings> {
    let file = load_file()?;
    let env_base_api_url = env::var(ENV_BASE_API_URL).ok();
    let env_base_model = env::var(ENV_BASE_MODEL).ok();
    resolve_non_secret_settings_from(
        &file,
        env_base_api_url.as_deref(),
        env_base_model.as_deref(),
    )
}

pub fn resolve_non_secret_settings_from(
    file: &FileConfig,
    env_base_api_url: Option<&str>,
    env_base_model: Option<&str>,
) -> Result<ResolvedNonSecretSettings> {
    let provider = if let Some(provider) = file.provider {
        ResolvedValue {
            value: provider,
            source: ValueSource::ConfigFile,
        }
    } else {
        ResolvedValue {
            value: Provider::default(),
            source: ValueSource::BuiltIn,
        }
    };

    let base_api_url = if let Some(url) = env_base_api_url {
        ResolvedValue {
            value: normalize_base_api_url(url)?,
            source: ValueSource::Environment,
        }
    } else if let Some(url) = file.base_api_url.as_deref() {
        ResolvedValue {
            value: normalize_base_api_url(url)?,
            source: ValueSource::ConfigFile,
        }
    } else {
        ResolvedValue {
            value: provider.value.default_base_api_url().to_string(),
            source: ValueSource::BuiltIn,
        }
    };

    let base_model = if let Some(model) = env_base_model {
        let model = model.trim();
        if model.is_empty() {
            bail!("BASE_MODEL cannot be empty");
        }
        ResolvedValue {
            value: model.to_string(),
            source: ValueSource::Environment,
        }
    } else if let Some(model) = file.base_model.as_deref() {
        let model = model.trim();
        if model.is_empty() {
            bail!("BASE_MODEL cannot be empty");
        }
        ResolvedValue {
            value: model.to_string(),
            source: ValueSource::ConfigFile,
        }
    } else {
        ResolvedValue {
            value: provider.value.default_base_model().to_string(),
            source: ValueSource::BuiltIn,
        }
    };

    let commit_style = if let Some(commit_style) = file.commit_style {
        ResolvedValue {
            value: commit_style,
            source: ValueSource::ConfigFile,
        }
    } else {
        ResolvedValue {
            value: CommitStyle::default(),
            source: ValueSource::BuiltIn,
        }
    };

    Ok(ResolvedNonSecretSettings {
        provider,
        base_api_url,
        base_model,
        commit_style,
    })
}

fn keyring_entry() -> Result<Entry> {
    Entry::new(KEYRING_SERVICE, KEYRING_USER).context("failed to access the keychain entry")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{
        CommitStyle, FileConfig, Provider, ValueSource, load_file_from_path,
        resolve_ai_settings_from, resolve_non_secret_settings_from, save_file_to_path,
    };

    #[test]
    fn resolves_from_config_file_and_keychain() {
        let resolved = resolve_ai_settings_from(
            &FileConfig {
                provider: Some(Provider::OpenAiCompatible),
                base_api_url: Some("https://example.com/v1/".into()),
                base_model: Some("example-model".into()),
                commit_style: Some(CommitStyle::Conventional),
            },
            None,
            None,
            None,
            Some("token-from-keychain"),
        )
        .unwrap();

        assert_eq!(resolved.api_token.source, ValueSource::Keychain);
        assert_eq!(resolved.provider.value, Provider::OpenAiCompatible);
        assert_eq!(resolved.base_api_url.source, ValueSource::ConfigFile);
        assert_eq!(resolved.base_api_url.value, "https://example.com/v1");
        assert_eq!(resolved.base_model.value, "example-model");
        assert_eq!(resolved.commit_style.value, CommitStyle::Conventional);
    }

    #[test]
    fn environment_overrides_config_values() {
        let resolved = resolve_ai_settings_from(
            &FileConfig {
                provider: Some(Provider::Gemini),
                base_api_url: Some("https://example.com/v1".into()),
                base_model: Some("from-config".into()),
                commit_style: Some(CommitStyle::Standard),
            },
            Some("env-token"),
            Some("https://override.example.com/api/"),
            Some("from-env"),
            Some("keychain-token"),
        )
        .unwrap();

        assert_eq!(resolved.api_token.source, ValueSource::Environment);
        assert_eq!(resolved.api_token.value, "env-token");
        assert_eq!(resolved.base_api_url.source, ValueSource::Environment);
        assert_eq!(
            resolved.base_api_url.value,
            "https://override.example.com/api"
        );
        assert_eq!(resolved.base_model.source, ValueSource::Environment);
        assert_eq!(resolved.base_model.value, "from-env");
        assert_eq!(resolved.commit_style.source, ValueSource::ConfigFile);
    }

    #[test]
    fn falls_back_to_built_in_defaults() {
        let resolved =
            resolve_ai_settings_from(&FileConfig::default(), Some("env-token"), None, None, None)
                .unwrap();

        assert_eq!(resolved.provider.source, ValueSource::BuiltIn);
        assert_eq!(resolved.provider.value, Provider::Gemini);
        assert_eq!(resolved.base_api_url.source, ValueSource::BuiltIn);
        assert_eq!(resolved.base_model.source, ValueSource::BuiltIn);
        assert_eq!(resolved.commit_style.source, ValueSource::BuiltIn);
        assert_eq!(resolved.commit_style.value, CommitStyle::Standard);
    }

    #[test]
    fn rejects_missing_token() {
        let error =
            resolve_ai_settings_from(&FileConfig::default(), None, None, None, None).unwrap_err();
        assert!(error.to_string().contains("auth login"));
    }

    #[test]
    fn reads_and_writes_config_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = FileConfig {
            provider: Some(Provider::Gemini),
            base_api_url: Some("https://example.com/v1".into()),
            base_model: Some("example-model".into()),
            commit_style: Some(CommitStyle::Conventional),
        };

        save_file_to_path(&path, &config).unwrap();
        let loaded = load_file_from_path(&path).unwrap();

        assert_eq!(loaded, config);
    }

    #[test]
    fn provider_changes_default_endpoint_and_model() {
        let resolved = resolve_non_secret_settings_from(
            &FileConfig {
                provider: Some(Provider::OpenAiCompatible),
                ..FileConfig::default()
            },
            None,
            None,
        )
        .unwrap();

        assert_eq!(resolved.provider.value, Provider::OpenAiCompatible);
        assert_eq!(resolved.base_api_url.value, "https://api.openai.com/v1");
        assert_eq!(resolved.base_model.value, "gpt-4.1-mini");
    }
}
