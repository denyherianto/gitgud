use std::{
    collections::BTreeMap,
    env, fmt, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use keyring::{Entry, Error as KeyringError};
use serde::{Deserialize, Serialize};

use crate::{ai::normalize_base_api_url, cli::ConfigKey};

const CONFIG_DIR_NAME: &str = "gitgud";
const CONFIG_FILE_NAME: &str = "config.toml";
const KEYRING_SERVICE: &str = "gitgud";
const KEYRING_USER: &str = "default";
const ENV_API_TOKEN: &str = "API_TOKEN";
const ENV_BASE_API_URL: &str = "BASE_API_URL";
const ENV_BASE_MODEL: &str = "BASE_MODEL";
pub const DEFAULT_CONVENTIONAL_PRESET_NAME: &str = "default";
pub const DEFAULT_CONVENTIONAL_TYPES: [&str; 9] = [
    "feat", "fix", "refactor", "docs", "test", "chore", "perf", "build", "ci",
];

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConventionalPreset {
    pub types: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConventionalCommitsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub presets: BTreeMap<String, ConventionalPreset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConventionalPreset {
    pub name: String,
    pub types: Vec<String>,
}

impl ResolvedConventionalPreset {
    pub fn built_in_default() -> Self {
        Self {
            name: DEFAULT_CONVENTIONAL_PRESET_NAME.to_string(),
            types: DEFAULT_CONVENTIONAL_TYPES
                .iter()
                .map(|commit_type| (*commit_type).to_string())
                .collect(),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conventional_commits: Option<ConventionalCommitsConfig>,
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
    pub conventional_preset: ResolvedValue<ResolvedConventionalPreset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedNonSecretSettings {
    pub provider: ResolvedValue<Provider>,
    pub base_api_url: ResolvedValue<String>,
    pub base_model: ResolvedValue<String>,
    pub commit_style: ResolvedValue<CommitStyle>,
    pub conventional_preset: ResolvedValue<ResolvedConventionalPreset>,
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
        ConfigKey::ConventionalPreset => {
            let conventional = config
                .conventional_commits
                .get_or_insert_with(Default::default);
            conventional.preset = Some(parse_conventional_preset_name(value)?);
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
        ConfigKey::ConventionalPreset => {
            if let Some(conventional) = &mut config.conventional_commits {
                conventional.preset = None;
            }
            prune_empty_conventional_config(&mut config);
        }
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

fn parse_conventional_preset_name(raw: &str) -> Result<String> {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        bail!("conventional preset name cannot be empty");
    }

    if !normalized
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
    {
        bail!("conventional preset names must use lowercase ASCII letters, digits, '-' or '_'");
    }

    Ok(normalized)
}

fn parse_conventional_type(raw: &str) -> Result<String> {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        bail!("conventional commit types cannot be empty");
    }

    if !normalized
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch == '-')
    {
        bail!("conventional commit types must use lowercase ASCII letters or hyphens");
    }

    Ok(normalized)
}

fn normalize_conventional_types(types: &[String]) -> Result<Vec<String>> {
    let mut normalized = Vec::with_capacity(types.len());
    for commit_type in types {
        let commit_type = parse_conventional_type(commit_type)?;
        if !normalized.iter().any(|existing| existing == &commit_type) {
            normalized.push(commit_type);
        }
    }

    if normalized.is_empty() {
        bail!("conventional presets must define at least one commit type");
    }

    Ok(normalized)
}

fn resolve_conventional_preset_from(
    file: &FileConfig,
) -> Result<ResolvedValue<ResolvedConventionalPreset>> {
    let Some(config) = file.conventional_commits.as_ref() else {
        return Ok(ResolvedValue {
            value: ResolvedConventionalPreset::built_in_default(),
            source: ValueSource::BuiltIn,
        });
    };

    let mut presets = BTreeMap::new();
    for (name, preset) in &config.presets {
        let name = parse_conventional_preset_name(name)?;
        if name == DEFAULT_CONVENTIONAL_PRESET_NAME {
            bail!("custom conventional preset name 'default' is reserved");
        }

        let types = normalize_conventional_types(&preset.types)
            .with_context(|| format!("invalid conventional preset '{name}'"))?;
        presets.insert(name.clone(), ResolvedConventionalPreset { name, types });
    }

    let Some(preset_name) = config.preset.as_deref() else {
        return Ok(ResolvedValue {
            value: ResolvedConventionalPreset::built_in_default(),
            source: ValueSource::BuiltIn,
        });
    };

    let preset_name = parse_conventional_preset_name(preset_name)?;
    if preset_name == DEFAULT_CONVENTIONAL_PRESET_NAME {
        return Ok(ResolvedValue {
            value: ResolvedConventionalPreset::built_in_default(),
            source: ValueSource::ConfigFile,
        });
    }

    let preset = presets
        .remove(&preset_name)
        .ok_or_else(|| anyhow::anyhow!("conventional preset '{preset_name}' is not defined"))?;
    Ok(ResolvedValue {
        value: preset,
        source: ValueSource::ConfigFile,
    })
}

fn prune_empty_conventional_config(config: &mut FileConfig) {
    if config
        .conventional_commits
        .as_ref()
        .is_some_and(|conventional| {
            conventional.preset.is_none() && conventional.presets.is_empty()
        })
    {
        config.conventional_commits = None;
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
        bail!("missing API token; run `gg config` or `gg auth login`, or set API_TOKEN");
    };

    Ok(ResolvedAiSettings {
        api_token,
        provider: non_secret.provider,
        base_api_url: non_secret.base_api_url,
        base_model: non_secret.base_model,
        commit_style: non_secret.commit_style,
        conventional_preset: non_secret.conventional_preset,
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
    let conventional_preset = resolve_conventional_preset_from(file)?;

    Ok(ResolvedNonSecretSettings {
        provider,
        base_api_url,
        base_model,
        commit_style,
        conventional_preset,
    })
}

fn keyring_entry() -> Result<Entry> {
    Entry::new(KEYRING_SERVICE, KEYRING_USER).context("failed to access the keychain entry")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::tempdir;

    use super::{
        CommitStyle, ConventionalCommitsConfig, ConventionalPreset,
        DEFAULT_CONVENTIONAL_PRESET_NAME, FileConfig, Provider, ValueSource, load_file_from_path,
        parse_commit_style, parse_provider, resolve_ai_settings_from,
        resolve_non_secret_settings_from, save_file_to_path,
    };

    #[test]
    fn reads_missing_config_file_as_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.toml");

        let loaded = load_file_from_path(&path).unwrap();

        assert_eq!(loaded, FileConfig::default());
    }

    #[test]
    fn resolves_from_config_file_and_keychain() {
        let resolved = resolve_ai_settings_from(
            &FileConfig {
                provider: Some(Provider::OpenAiCompatible),
                base_api_url: Some("https://example.com/v1/".into()),
                base_model: Some("example-model".into()),
                commit_style: Some(CommitStyle::Conventional),
                ..FileConfig::default()
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
        assert_eq!(
            resolved.conventional_preset.value.name,
            DEFAULT_CONVENTIONAL_PRESET_NAME
        );
    }

    #[test]
    fn environment_overrides_config_values() {
        let resolved = resolve_ai_settings_from(
            &FileConfig {
                provider: Some(Provider::Gemini),
                base_api_url: Some("https://example.com/v1".into()),
                base_model: Some("from-config".into()),
                commit_style: Some(CommitStyle::Standard),
                ..FileConfig::default()
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
        assert_eq!(resolved.conventional_preset.source, ValueSource::BuiltIn);
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
        assert_eq!(
            resolved.conventional_preset.value.name,
            DEFAULT_CONVENTIONAL_PRESET_NAME
        );
    }

    #[test]
    fn rejects_missing_token() {
        let error =
            resolve_ai_settings_from(&FileConfig::default(), None, None, None, None).unwrap_err();
        assert!(error.to_string().contains("auth login"));
    }

    #[test]
    fn rejects_empty_environment_token() {
        let error = resolve_ai_settings_from(&FileConfig::default(), Some("   "), None, None, None)
            .unwrap_err();

        assert!(error.to_string().contains("API_TOKEN cannot be empty"));
    }

    #[test]
    fn rejects_empty_keychain_token() {
        let error = resolve_ai_settings_from(&FileConfig::default(), None, None, None, Some(" "))
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("stored API token cannot be empty")
        );
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
            conventional_commits: Some(ConventionalCommitsConfig {
                preset: Some("team".into()),
                presets: BTreeMap::from([(
                    "team".into(),
                    ConventionalPreset {
                        types: vec!["feat".into(), "fix".into()],
                    },
                )]),
            }),
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
        assert_eq!(
            resolved.conventional_preset.value.name,
            DEFAULT_CONVENTIONAL_PRESET_NAME
        );
    }

    #[test]
    fn parses_provider_aliases() {
        assert_eq!(
            parse_provider("openai_compatible").unwrap(),
            Provider::OpenAiCompatible
        );
        assert_eq!(
            parse_provider("OpenAICompatible").unwrap(),
            Provider::OpenAiCompatible
        );
    }

    #[test]
    fn rejects_unknown_commit_style() {
        let error = parse_commit_style("squash").unwrap_err();
        assert!(error.to_string().contains("commit style"));
    }

    #[test]
    fn resolves_custom_conventional_preset() {
        let resolved = resolve_non_secret_settings_from(
            &FileConfig {
                commit_style: Some(CommitStyle::Conventional),
                conventional_commits: Some(ConventionalCommitsConfig {
                    preset: Some("backend".into()),
                    presets: BTreeMap::from([(
                        "backend".into(),
                        ConventionalPreset {
                            types: vec!["feat".into(), "bugfix".into(), "bugfix".into()],
                        },
                    )]),
                }),
                ..FileConfig::default()
            },
            None,
            None,
        )
        .unwrap();

        assert_eq!(resolved.conventional_preset.source, ValueSource::ConfigFile);
        assert_eq!(resolved.conventional_preset.value.name, "backend");
        assert_eq!(
            resolved.conventional_preset.value.types,
            vec!["feat".to_string(), "bugfix".to_string()]
        );
    }

    #[test]
    fn rejects_missing_custom_conventional_preset() {
        let error = resolve_non_secret_settings_from(
            &FileConfig {
                conventional_commits: Some(ConventionalCommitsConfig {
                    preset: Some("missing".into()),
                    presets: BTreeMap::new(),
                }),
                ..FileConfig::default()
            },
            None,
            None,
        )
        .unwrap_err();

        assert!(error.to_string().contains("not defined"));
    }

    #[test]
    fn rejects_empty_environment_base_model() {
        let error = resolve_non_secret_settings_from(&FileConfig::default(), None, Some("   "))
            .unwrap_err();

        assert!(error.to_string().contains("BASE_MODEL cannot be empty"));
    }

    #[test]
    fn rejects_empty_config_base_model() {
        let error = resolve_non_secret_settings_from(
            &FileConfig {
                base_model: Some(" ".into()),
                ..FileConfig::default()
            },
            None,
            None,
        )
        .unwrap_err();

        assert!(error.to_string().contains("BASE_MODEL cannot be empty"));
    }
}
