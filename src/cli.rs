use clap::{Parser, Subcommand, ValueEnum};

use crate::config::{CommitStyle, Provider};

#[derive(Debug, Parser)]
#[command(
    name = "gg",
    about = "A Git CLI with AI-assisted commit, push, and setup flows"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Generate 3-5 commit message options for staged changes
    Commit,
    /// Push the current branch automatically and confirm force-with-lease only when needed
    Push,
    /// Open setup or manage persistent configuration
    Config {
        #[command(subcommand)]
        command: Option<ConfigCommand>,
    },
    /// Manage secure authentication storage
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Validate git and AI provider configuration
    Doctor,
}

#[derive(Debug, Clone, Subcommand)]
pub enum ConfigCommand {
    /// Open the interactive setup screen
    Setup,
    /// Show the effective configuration and where it comes from
    Show,
    /// Persist a global configuration value
    Set { key: ConfigKey, value: String },
    /// Remove a persisted global configuration value
    Unset { key: ConfigKey },
}

#[derive(Debug, Clone, Subcommand)]
pub enum AuthCommand {
    /// Store the API token in the system keychain
    Login {
        #[arg(long)]
        token: Option<String>,
    },
    /// Show whether an API token is available
    Status,
    /// Remove the stored API token from the system keychain
    Logout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ConfigKey {
    Provider,
    BaseApiUrl,
    BaseModel,
    CommitStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProviderArg {
    Gemini,
    OpenAiCompatible,
}

impl From<ProviderArg> for Provider {
    fn from(value: ProviderArg) -> Self {
        match value {
            ProviderArg::Gemini => Provider::Gemini,
            ProviderArg::OpenAiCompatible => Provider::OpenAiCompatible,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CommitStyleArg {
    Standard,
    Conventional,
}

impl From<CommitStyleArg> for CommitStyle {
    fn from(value: CommitStyleArg) -> Self {
        match value {
            CommitStyleArg::Standard => CommitStyle::Standard,
            CommitStyleArg::Conventional => CommitStyle::Conventional,
        }
    }
}
