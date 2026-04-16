use std::{env, error::Error as StdError, ffi::OsString, fmt};

use anyhow::{Result, bail};
use clap::Parser;
use rpassword::prompt_password;

use crate::{
    ai::{AiClient, AiConfig},
    cli::{AuthCommand, Cli, Command, ConfigCommand},
    config::{
        TokenStatus, config_path, delete_api_token, load_file, resolve_ai_settings,
        resolve_non_secret_settings, save_file_to_path, set_config_value, store_api_token,
        token_status, unset_config_value,
    },
    git::{GitRepo, push_needs_force_with_lease},
    tui::{self, CommitAction, ConfigSetupAction, ConfigSetupInput, HomeAction},
};

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    let cwd = env::current_dir()?;
    let repo = GitRepo::new(cwd);

    let command = match cli.command {
        Some(command) => Some(command),
        None => resolve_home_command(&repo)?,
    };

    let Some(command) = command else {
        return Ok(());
    };

    match command {
        Command::Commit => run_commit(&repo).await,
        Command::Push => run_push(&repo),
        Command::Git { args } => run_git_passthrough(&repo, &args),
        Command::Passthrough(args) => run_git_passthrough(&repo, &args),
        Command::Config { command } => run_config(command),
        Command::Auth { command } => run_auth(command),
        Command::Doctor => run_doctor(&repo).await,
    }
}

fn resolve_home_command(repo: &GitRepo) -> Result<Option<Command>> {
    repo.ensure_git_available()?;
    repo.ensure_repo()?;

    let status = repo.status()?;
    Ok(match tui::run_home(&status)? {
        HomeAction::Commit => Some(Command::Commit),
        HomeAction::Push => Some(Command::Push),
        HomeAction::Quit => None,
    })
}

async fn run_commit(repo: &GitRepo) -> Result<()> {
    repo.ensure_git_available()?;
    repo.ensure_repo()?;

    let status = repo.status()?;
    if status.staged_count == 0 {
        if status.unstaged_count > 0 {
            if !tui::confirm_stage_all_changes()? {
                println!("commit cancelled");
                return Ok(());
            }

            repo.stage_all()?;
        } else {
            let message = "No staged changes found. Stage files before generating commit messages.";
            tui::show_message("Cannot Commit", message)?;
            bail!(message);
        }
    }

    let config = AiConfig::load()?;
    let client = AiClient::new(config.clone())?;
    match tui::run_commit(
        repo,
        client,
        config.commit_style,
        config.conventional_preset,
    )
    .await?
    {
        CommitAction::Confirmed(message) => {
            let output = repo.commit(&message)?;
            if !output.trim().is_empty() {
                println!("{output}");
            } else {
                println!("commit created");
            }
            Ok(())
        }
        CommitAction::Cancelled => {
            println!("commit cancelled");
            Ok(())
        }
    }
}

fn run_push(repo: &GitRepo) -> Result<()> {
    repo.ensure_git_available()?;
    repo.ensure_repo()?;

    let plan = repo.plan_push().or_else(|error| {
        tui::show_message("Cannot Push", &error.to_string())?;
        Err(error)
    })?;

    match repo.push(&plan) {
        Ok(output) => {
            if !output.trim().is_empty() {
                println!("{output}");
            } else {
                println!("push completed");
            }
            Ok(())
        }
        Err(error) => {
            let rendered = error.to_string();
            if !push_needs_force_with_lease(&rendered) {
                return Err(error);
            }

            if !tui::confirm_force_push(&plan, &rendered)? {
                println!("push cancelled");
                return Ok(());
            }

            let output = repo.push_with_force_lease(&plan)?;
            if !output.trim().is_empty() {
                println!("{output}");
            } else {
                println!("push completed with --force-with-lease");
            }
            Ok(())
        }
    }
}

fn run_git_passthrough(repo: &GitRepo, args: &[OsString]) -> Result<()> {
    repo.ensure_git_available()?;

    let status = repo.run_passthrough(args)?;
    if status.success() {
        Ok(())
    } else {
        Err(GitPassthroughExit {
            code: status.code().unwrap_or(1),
        }
        .into())
    }
}

fn run_config(command: Option<ConfigCommand>) -> Result<()> {
    match command {
        None | Some(ConfigCommand::Setup) => run_config_setup(),
        Some(ConfigCommand::Show) => {
            let path = config_path()?;
            let stored = load_file()?;
            let non_secret = resolve_non_secret_settings()?;
            println!("Config path: {}", path.display());
            println!(
                "Stored provider: {}",
                stored
                    .provider
                    .map(|provider| provider.to_string())
                    .unwrap_or_else(|| "(not set)".to_string())
            );
            println!(
                "Stored base_api_url: {}",
                stored.base_api_url.as_deref().unwrap_or("(not set)")
            );
            println!(
                "Stored base_model: {}",
                stored.base_model.as_deref().unwrap_or("(not set)")
            );
            println!(
                "Stored commit_style: {}",
                stored
                    .commit_style
                    .map(|commit_style| commit_style.to_string())
                    .unwrap_or_else(|| "(not set)".to_string())
            );
            println!(
                "Stored conventional_preset: {}",
                stored
                    .conventional_commits
                    .as_ref()
                    .and_then(|conventional| conventional.preset.as_deref())
                    .unwrap_or("(not set)")
            );
            println!(
                "Effective provider: {} ({})",
                non_secret.provider.value, non_secret.provider.source
            );
            println!(
                "Effective base_api_url: {} ({})",
                non_secret.base_api_url.value, non_secret.base_api_url.source
            );
            println!(
                "Effective base_model: {} ({})",
                non_secret.base_model.value, non_secret.base_model.source
            );
            println!(
                "Effective commit_style: {} ({})",
                non_secret.commit_style.value, non_secret.commit_style.source
            );
            println!(
                "Effective conventional_preset: {} ({})",
                non_secret.conventional_preset.value.name, non_secret.conventional_preset.source
            );
            println!(
                "Effective conventional_types: {}",
                non_secret.conventional_preset.value.types.join(", ")
            );

            match token_status()? {
                TokenStatus::EnvironmentOverride => {
                    println!("API token source: environment");
                }
                TokenStatus::Keychain => {
                    println!("API token source: system keychain");
                }
                TokenStatus::Missing => {
                    println!("API token source: (not configured)");
                }
            }

            Ok(())
        }
        Some(ConfigCommand::Set { key, value }) => {
            let path = set_config_value(key, &value)?;
            println!("Updated {} in {}", config_key_name(key), path.display());
            Ok(())
        }
        Some(ConfigCommand::Unset { key }) => {
            let path = unset_config_value(key)?;
            println!("Removed {} from {}", config_key_name(key), path.display());
            Ok(())
        }
    }
}

fn run_config_setup() -> Result<()> {
    let path = config_path()?;
    let resolved = resolve_non_secret_settings()?;
    let current_token_status = token_status()?;
    let input = ConfigSetupInput {
        provider: resolved.provider.value,
        base_api_url: resolved.base_api_url.value,
        base_model: resolved.base_model.value,
        commit_style: resolved.commit_style.value,
        token_status: current_token_status.clone(),
        token_present: !matches!(current_token_status, TokenStatus::Missing),
    };

    let Some(ConfigSetupAction {
        provider,
        base_api_url,
        base_model,
        commit_style,
        api_token,
    }) = tui::run_config_setup(input)?
    else {
        println!("config cancelled");
        return Ok(());
    };

    let base_model = base_model.trim();
    if base_model.is_empty() {
        bail!("BASE_MODEL cannot be empty");
    }

    let mut config = load_file()?;
    config.provider = Some(provider);
    config.base_api_url = Some(base_api_url);
    config.base_model = Some(base_model.to_string());
    config.commit_style = Some(commit_style);
    save_file_to_path(&path, &config)?;

    if let Some(token) = api_token {
        store_api_token(&token)?;
    }

    println!("Updated configuration in {}", path.display());
    Ok(())
}

fn run_auth(command: AuthCommand) -> Result<()> {
    match command {
        AuthCommand::Login { token } => {
            let token = match token {
                Some(token) => token,
                None => prompt_password("API token: ")?,
            };

            store_api_token(&token)?;
            println!("Stored API token in the system keychain");
            Ok(())
        }
        AuthCommand::Status => {
            match token_status()? {
                TokenStatus::EnvironmentOverride => {
                    println!("API token available from environment; it overrides the keychain");
                }
                TokenStatus::Keychain => {
                    println!("API token available from the system keychain");
                }
                TokenStatus::Missing => {
                    println!("No API token configured; run `gg config` or `gg auth login`");
                }
            }
            Ok(())
        }
        AuthCommand::Logout => {
            if delete_api_token()? {
                println!("Removed API token from the system keychain");
            } else {
                println!("No stored API token found in the system keychain");
            }
            Ok(())
        }
    }
}

fn config_key_name(key: crate::cli::ConfigKey) -> &'static str {
    match key {
        crate::cli::ConfigKey::Provider => "provider",
        crate::cli::ConfigKey::BaseApiUrl => "base-api-url",
        crate::cli::ConfigKey::BaseModel => "base-model",
        crate::cli::ConfigKey::CommitStyle => "commit-style",
        crate::cli::ConfigKey::ConventionalPreset => "conventional-preset",
    }
}

async fn run_doctor(repo: &GitRepo) -> Result<()> {
    let mut failures = 0;

    match repo.ensure_git_available() {
        Ok(()) => println!("[ok] git binary is available"),
        Err(error) => {
            failures += 1;
            println!("[fail] git binary check: {error}");
        }
    }

    match repo.ensure_repo() {
        Ok(()) => println!("[ok] current directory is a git repository"),
        Err(error) => {
            failures += 1;
            println!("[fail] repository check: {error}");
        }
    }

    match resolve_ai_settings() {
        Ok(resolved) => {
            println!("[ok] AI token available from {}", resolved.api_token.source);
            println!("[ok] provider resolved from {}", resolved.provider.source);
            println!(
                "[ok] BASE_API_URL resolved from {}",
                resolved.base_api_url.source
            );
            println!(
                "[ok] BASE_MODEL resolved from {}",
                resolved.base_model.source
            );
            println!(
                "[ok] commit style resolved from {}",
                resolved.commit_style.source
            );
            println!(
                "[ok] conventional preset resolved from {}",
                resolved.conventional_preset.source
            );

            let config = AiConfig::load()?;
            match AiClient::new(config.clone())?.check_reachability().await {
                Ok(status) => println!("[ok] BASE_API_URL reachable (HTTP {status})"),
                Err(error) => {
                    failures += 1;
                    println!("[fail] BASE_API_URL reachability: {error}");
                }
            }
            println!("[ok] provider set to {}", config.provider);
            println!("[ok] BASE_MODEL set to {}", config.base_model);
            println!("[ok] commit style set to {}", config.commit_style);
            println!(
                "[ok] conventional preset set to {} [{}]",
                config.conventional_preset.name,
                config.conventional_preset.types.join(", ")
            );
        }
        Err(error) => {
            failures += 1;
            println!("[fail] AI configuration: {error}");
        }
    }

    if failures > 0 {
        bail!("doctor found {failures} issue(s)");
    }

    println!("doctor checks passed");
    Ok(())
}

#[derive(Debug)]
pub struct GitPassthroughExit {
    code: i32,
}

impl GitPassthroughExit {
    pub fn code(&self) -> i32 {
        self.code
    }
}

impl fmt::Display for GitPassthroughExit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "git exited with status {}", self.code)
    }
}

impl StdError for GitPassthroughExit {}
