use std::ffi::OsString;

use clap::{Parser, Subcommand, ValueEnum};

use crate::config::{CommitStyle, Provider};
use crate::rescue::RescueIncident;

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
    /// Generate 1-3 commit message options for staged changes
    Commit,
    /// Preflight a branch, clean up commits, draft review text, and push
    Ship,
    /// Explain the staged diff, including changes, intent, risks, and tests
    Explain,
    /// Push the current branch automatically and confirm force-with-lease only when needed
    Push,
    /// Diagnose and recover common Git mistakes with guided rescue flows
    Rescue {
        #[arg(value_enum)]
        incident: Option<RescueIncident>,
    },
    /// Run a raw git command, including built-in names like `commit` or `push`
    Git {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
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
    /// Ask a question in natural language and get suggested git commands
    Ask {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        query: Vec<String>,
    },
    #[command(external_subcommand)]
    Passthrough(Vec<OsString>),
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
    GenerationMode,
    ConventionalPreset,
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, Command};

    #[test]
    fn parses_unknown_subcommand_as_git_passthrough() {
        let cli = Cli::try_parse_from(["gg", "status", "--short"]).unwrap();

        match cli.command {
            Some(Command::Passthrough(args)) => {
                assert_eq!(args.len(), 2);
                assert_eq!(args[0].to_string_lossy(), "status");
                assert_eq!(args[1].to_string_lossy(), "--short");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_explicit_git_passthrough() {
        let cli = Cli::try_parse_from(["gg", "git", "commit", "--amend"]).unwrap();

        match cli.command {
            Some(Command::Git { args }) => {
                assert_eq!(args.len(), 2);
                assert_eq!(args[0].to_string_lossy(), "commit");
                assert_eq!(args[1].to_string_lossy(), "--amend");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_explain_command() {
        let cli = Cli::try_parse_from(["gg", "explain"]).unwrap();

        match cli.command {
            Some(Command::Explain) => {}
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_ship_command() {
        let cli = Cli::try_parse_from(["gg", "ship"]).unwrap();

        match cli.command {
            Some(Command::Ship) => {}
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_rescue_command() {
        let cli = Cli::try_parse_from(["gg", "rescue", "detached-head"]).unwrap();

        match cli.command {
            Some(Command::Rescue { incident }) => {
                assert!(incident.is_some());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}
