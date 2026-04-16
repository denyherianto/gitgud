use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "git-buddy", about = "A Git CLI with an AI-assisted TUI")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Generate and confirm a commit message for staged changes
    Commit,
    /// Push the current branch to its upstream or choose a remote
    Push,
    /// Manage persistent configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
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
    BaseApiUrl,
    BaseModel,
}
