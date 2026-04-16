use std::env;

use anyhow::{Result, bail};
use clap::Parser;
use rpassword::prompt_password;

use crate::{
    ai::{AiClient, AiConfig},
    cli::{AuthCommand, Cli, Command, ConfigCommand},
    config::{
        TokenStatus, config_path, delete_api_token, load_file, resolve_ai_settings,
        resolve_non_secret_settings, set_config_value, store_api_token, token_status,
        unset_config_value,
    },
    git::{GitRepo, PushPlan},
    tui::{self, CommitAction, HomeAction, PushAction},
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

    let config = AiConfig::load()?;
    let client = AiClient::new(config)?;
    match tui::run_commit(repo, client).await? {
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

    match tui::run_push(repo)? {
        PushAction::Confirmed(plan) => {
            let output = match &plan {
                PushPlan::ChooseRemote { .. } => unreachable!(),
                PushPlan::Upstream { .. } | PushPlan::SetUpstream { .. } => repo.push(&plan)?,
            };

            if !output.trim().is_empty() {
                println!("{output}");
            } else {
                println!("push completed");
            }

            Ok(())
        }
        PushAction::Cancelled => {
            println!("push cancelled");
            Ok(())
        }
    }
}

fn run_config(command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Show => {
            let path = config_path()?;
            let stored = load_file()?;
            let non_secret = resolve_non_secret_settings()?;
            println!("Config path: {}", path.display());
            println!(
                "Stored base_api_url: {}",
                stored.base_api_url.as_deref().unwrap_or("(not set)")
            );
            println!(
                "Stored base_model: {}",
                stored.base_model.as_deref().unwrap_or("(not set)")
            );
            println!(
                "Effective base_api_url: {} ({})",
                non_secret.base_api_url.value, non_secret.base_api_url.source
            );
            println!(
                "Effective base_model: {} ({})",
                non_secret.base_model.value, non_secret.base_model.source
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
        ConfigCommand::Set { key, value } => {
            let path = set_config_value(key, &value)?;
            println!("Updated {} in {}", config_key_name(key), path.display());
            Ok(())
        }
        ConfigCommand::Unset { key } => {
            let path = unset_config_value(key)?;
            println!("Removed {} from {}", config_key_name(key), path.display());
            Ok(())
        }
    }
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
                    println!("No API token configured; run `git-buddy auth login`");
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
        crate::cli::ConfigKey::BaseApiUrl => "base-api-url",
        crate::cli::ConfigKey::BaseModel => "base-model",
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
            println!(
                "[ok] BASE_API_URL resolved from {}",
                resolved.base_api_url.source
            );
            println!(
                "[ok] BASE_MODEL resolved from {}",
                resolved.base_model.source
            );

            let config = AiConfig::load()?;
            match AiClient::new(config.clone())?.check_reachability().await {
                Ok(status) => println!("[ok] BASE_API_URL reachable (HTTP {status})"),
                Err(error) => {
                    failures += 1;
                    println!("[fail] BASE_API_URL reachability: {error}");
                }
            }
            println!("[ok] BASE_MODEL set to {}", config.base_model);
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
