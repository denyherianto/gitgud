use std::{env, error::Error as StdError, ffi::OsString, fmt};

use anyhow::{Result, bail};
use clap::Parser;
use rpassword::prompt_password;

use crate::{
    ai::{
        AiClient, AiConfig, AskContext, DiffExplanation, PromptInput, ShipCommit, ShipPlan,
        ShipPromptInput, SuggestedCommand, build_heuristic_ship_plan,
    },
    cli::{AuthCommand, Cli, Command, ConfigCommand},
    config::{
        GenerationMode, TokenStatus, config_path, delete_api_token, load_api_token, load_file,
        resolve_ai_settings, resolve_non_secret_settings, save_file_to_path, set_config_value,
        store_api_token, token_status, unset_config_value,
    },
    git::{GitRepo, PushPlan, push_needs_force_with_lease},
    risk,
    tui::{self, AskAction, CommitAction, ConfigSetupAction, ConfigSetupInput, HomeAction},
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
        Command::Ship => run_ship(&repo).await,
        Command::Explain => run_explain(&repo).await,
        Command::Push => run_push(&repo),
        Command::Git { args } => run_git_passthrough(&repo, &args),
        Command::Ask { query } => run_ask_command(&repo, &query.join(" ")).await,
        Command::Passthrough(args) => {
            let first = args
                .first()
                .map(|a| a.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if !first.is_empty() && !is_known_git_subcommand(&first) {
                let query = args
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(" ");
                run_ask_command(&repo, &query).await
            } else {
                run_git_passthrough(&repo, &args)
            }
        }
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
        HomeAction::Ship => Some(Command::Ship),
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

    let warnings = repo.staged_diff_warnings()?;
    if !warnings.is_empty()
        && !tui::confirm_unsafe_diff_warnings("generating commit messages", &warnings)?
    {
        println!("commit cancelled");
        return Ok(());
    }

    let settings = resolve_non_secret_settings()?;
    let generator = match settings.generation_mode.value {
        GenerationMode::HeuristicOnly => tui::CommitGenerator::HeuristicOnly,
        GenerationMode::Auto | GenerationMode::AiOnly => {
            let config = AiConfig::load()?;
            tui::CommitGenerator::Ai(AiClient::new(config)?)
        }
    };

    execute_commit_action(
        repo,
        tui::run_commit(
            repo,
            generator,
            settings.commit_style.value,
            settings.generation_mode.value,
            settings.conventional_preset.value,
        )
        .await?,
    )
}

async fn run_ship(repo: &GitRepo) -> Result<()> {
    repo.ensure_git_available()?;
    repo.ensure_repo()?;

    let initial_status = repo.status()?;
    if initial_status.staged_count > 0 || initial_status.unstaged_count > 0 {
        println!(
            "preflight: {} staged, {} unstaged",
            initial_status.staged_count, initial_status.unstaged_count
        );
        run_commit(repo).await?;
    }

    let status = repo.status()?;
    if (status.staged_count > 0 || status.unstaged_count > 0)
        && !tui::confirm_message(
            "Ship Preflight",
            &format!(
                "{} staged file(s) and {} unstaged file(s) are still outside the branch you are about to ship.\n\nPress Enter to continue with the current commit stack, or Esc to cancel.",
                status.staged_count, status.unstaged_count
            ),
        )?
    {
        println!("ship cancelled");
        return Ok(());
    }

    let plan = repo.plan_push().or_else(|error| {
        tui::show_message("Cannot Ship", &error.to_string())?;
        Err(error)
    })?;
    let ship_range = repo.ship_range(&plan)?;
    if ship_range.commits.is_empty() {
        let message = "No local commits found to ship from the current branch.";
        tui::show_message("Nothing To Ship", message)?;
        println!("{message}");
        return Ok(());
    }

    let warnings = repo.push_diff_warnings(&plan)?;
    if !warnings.is_empty() && !tui::confirm_unsafe_diff_warnings("shipping", &warnings)? {
        println!("ship cancelled");
        return Ok(());
    }

    let ship_plan = build_ship_plan(
        &status,
        &ship_range.base_label,
        &ship_range.commits,
        &ship_range.changed_files,
        &ship_range.diff_stat,
        &ship_range.diff,
    )
    .await?;
    print_ship_plan(&status, &plan, &ship_range.base_label, &ship_plan);

    if has_cleanup_suggestions(&ship_plan)
        && !tui::confirm_message(
            "Commit Cleanup Suggestions",
            "gitgud found split, squash, or cleanup suggestions for this branch.\n\nPress Enter to keep shipping with the current history, or Esc to cancel and clean it up first.",
        )?
    {
        println!("ship cancelled");
        return Ok(());
    }

    if !tui::confirm_message(
        "Ready To Push",
        "Press Enter to push this branch now, or Esc to cancel.",
    )? {
        println!("ship cancelled");
        return Ok(());
    }

    execute_push_plan(repo, &plan)?;

    println!();
    println!("PR title: {}", ship_plan.pr_title);
    println!();
    println!("PR body:");
    println!("{}", ship_plan.pr_body);

    if repo.has_gh_cli() && repo.has_github_remote()? {
        if tui::confirm_message(
            "Create Pull Request",
            "The branch is pushed and a GitHub remote is available.\n\nPress Enter to create a pull request with the generated title and body, or Esc to skip.",
        )? {
            let output = repo.create_pull_request(&ship_plan.pr_title, &ship_plan.pr_body)?;
            if !output.trim().is_empty() {
                println!();
                println!("{output}");
            } else {
                println!("pull request created");
            }
        } else {
            println!("pull request skipped");
        }
    } else {
        println!("pull request draft generated; GitHub CLI or GitHub remote not available");
    }

    Ok(())
}

fn execute_commit_action(repo: &GitRepo, action: CommitAction) -> Result<()> {
    match action {
        CommitAction::Confirmed(message) => {
            let output = repo.commit(&message)?;
            if !output.trim().is_empty() {
                println!("{output}");
            } else {
                println!("commit created");
            }
            Ok(())
        }
        CommitAction::SplitRequested(plans) => {
            if !tui::confirm_split_commits(&plans)? {
                println!("split commit cancelled");
                return Ok(());
            }

            repo.split_commit(&plans)?;
            println!("created {} split commits", plans.len());
            for plan in plans {
                println!("- {}", plan.message.lines().next().unwrap_or("(empty)"));
            }
            Ok(())
        }
        CommitAction::Cancelled => {
            println!("commit cancelled");
            Ok(())
        }
    }
}

async fn build_ship_plan(
    status: &crate::git::RepoStatus,
    base_label: &str,
    commits: &[crate::git::BranchCommit],
    changed_files: &[String],
    diff_stat: &str,
    diff: &str,
) -> Result<ShipPlan> {
    let input = ShipPromptInput {
        branch: status
            .branch
            .clone()
            .unwrap_or_else(|| "DETACHED".to_string()),
        base_label: base_label.to_string(),
        staged_count: status.staged_count,
        unstaged_count: status.unstaged_count,
        local_commits: commits
            .iter()
            .map(|commit| ShipCommit {
                subject: commit.subject.clone(),
                body: commit.body.clone(),
            })
            .collect(),
        changed_files: changed_files.to_vec(),
        diff_stat: diff_stat.to_string(),
        diff: diff.to_string(),
    };
    let settings = resolve_non_secret_settings()?;

    match settings.generation_mode.value {
        GenerationMode::HeuristicOnly => Ok(build_heuristic_ship_plan(&input)),
        GenerationMode::Auto | GenerationMode::AiOnly => {
            let config = AiConfig::load()?;
            let client = AiClient::new(config)?;
            client.generate_ship_plan(&input).await
        }
    }
}

fn has_cleanup_suggestions(plan: &ShipPlan) -> bool {
    !(plan.commit_cleanup.is_empty()
        && plan.split_suggestions.is_empty()
        && plan.squash_suggestions.is_empty())
}

fn print_ship_plan(
    status: &crate::git::RepoStatus,
    plan: &PushPlan,
    base_label: &str,
    ship_plan: &ShipPlan,
) {
    println!("Ship preflight:");
    println!(
        "- branch: {}",
        status.branch.as_deref().unwrap_or("DETACHED")
    );
    println!("- compare against: {base_label}");
    println!(
        "- push target: {}",
        match plan {
            PushPlan::Upstream { branch } => format!("upstream for {branch}"),
            PushPlan::SetUpstream { remote, branch } => format!("{remote}/{branch}"),
        }
    );
    if let Some(note) = &ship_plan.note {
        println!("- note: {note}");
    }
    print_ship_section("Checks", &ship_plan.preflight_checks);
    print_ship_section("Commit Cleanup", &ship_plan.commit_cleanup);
    print_ship_section("Split Suggestions", &ship_plan.split_suggestions);
    print_ship_section("Squash Suggestions", &ship_plan.squash_suggestions);
    println!("PR draft:");
    println!("- title: {}", ship_plan.pr_title);
    println!("{}", ship_plan.pr_body);
}

fn print_ship_section(title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }

    println!("{title}:");
    for item in items {
        println!("- {item}");
    }
}

fn run_push(repo: &GitRepo) -> Result<()> {
    repo.ensure_git_available()?;
    repo.ensure_repo()?;

    let plan = repo.plan_push().or_else(|error| {
        tui::show_message("Cannot Push", &error.to_string())?;
        Err(error)
    })?;
    let warnings = repo.push_diff_warnings(&plan)?;
    if !warnings.is_empty() && !tui::confirm_unsafe_diff_warnings("pushing", &warnings)? {
        println!("push cancelled");
        return Ok(());
    }

    execute_push_plan(repo, &plan)
}

fn execute_push_plan(repo: &GitRepo, plan: &PushPlan) -> Result<()> {
    match repo.push(plan) {
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

            if !tui::confirm_force_push(plan, &rendered)? {
                println!("push cancelled");
                return Ok(());
            }

            let output = repo.push_with_force_lease(plan)?;
            if !output.trim().is_empty() {
                println!("{output}");
            } else {
                println!("push completed with --force-with-lease");
            }
            Ok(())
        }
    }
}
async fn run_explain(repo: &GitRepo) -> Result<()> {
    repo.ensure_git_available()?;
    repo.ensure_repo()?;

    let staged = repo.staged_changes()?;
    if staged.staged_files.is_empty() {
        let message = "No staged changes found. Stage files before explaining the diff.";
        tui::show_message("Cannot Explain Diff", message)?;
        bail!(message);
    }

    let config = AiConfig::load()?;
    let input = PromptInput {
        branch: repo
            .branch_name()?
            .unwrap_or_else(|| "DETACHED".to_string()),
        staged_files: staged.staged_files,
        diff_stat: staged.diff_stat,
        diff: staged.diff,
        commit_style: config.commit_style,
        conventional_preset: config.conventional_preset.clone(),
    };
    let client = AiClient::new(config)?;
    let explanation = client.generate_diff_explanation(&input).await?;

    print_diff_explanation(&explanation);
    Ok(())
}

fn print_diff_explanation(explanation: &DiffExplanation) {
    print_diff_explanation_section("What Changed", &explanation.what_changed);
    println!();
    print_diff_explanation_section("Possible Intent", &explanation.possible_intent);
    println!();
    print_diff_explanation_section("Risk Areas", &explanation.risk_areas);
    println!();
    print_diff_explanation_section("Test Suggestions", &explanation.test_suggestions);
}

fn print_diff_explanation_section(title: &str, items: &[String]) {
    println!("{title}:");
    for item in items {
        println!("- {item}");
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
                "Stored generation_mode: {}",
                stored
                    .generation_mode
                    .map(|generation_mode| generation_mode.to_string())
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
                "Effective generation_mode: {} ({})",
                non_secret.generation_mode.value, non_secret.generation_mode.source
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
    let existing_api_token = env::var("API_TOKEN")
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
        .or(load_api_token()?);
    let input = ConfigSetupInput {
        provider: resolved.provider.value,
        base_api_url: resolved.base_api_url.value,
        base_model: resolved.base_model.value,
        commit_style: resolved.commit_style.value,
        generation_mode: resolved.generation_mode.value,
        existing_api_token,
        token_status: current_token_status.clone(),
        token_present: !matches!(current_token_status, TokenStatus::Missing),
    };

    let Some(ConfigSetupAction {
        provider,
        base_api_url,
        base_model,
        commit_style,
        generation_mode,
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
    config.generation_mode = Some(generation_mode);
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
        crate::cli::ConfigKey::GenerationMode => "generation-mode",
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

    match resolve_non_secret_settings() {
        Ok(resolved) => {
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
                "[ok] generation mode resolved from {}",
                resolved.generation_mode.source
            );
            println!(
                "[ok] conventional preset resolved from {}",
                resolved.conventional_preset.source
            );
            match resolve_ai_settings() {
                Ok(ai_resolved) => {
                    println!(
                        "[ok] AI token available from {}",
                        ai_resolved.api_token.source
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
                    println!("[ok] generation mode set to {}", config.generation_mode);
                    println!(
                        "[ok] conventional preset set to {} [{}]",
                        config.conventional_preset.name,
                        config.conventional_preset.types.join(", ")
                    );
                }
                Err(error)
                    if matches!(
                        resolved.generation_mode.value,
                        GenerationMode::HeuristicOnly
                    ) && error.to_string().contains("missing API token") =>
                {
                    println!("[ok] AI token not required for heuristic-only commit generation");
                }
                Err(error) => {
                    failures += 1;
                    println!("[fail] AI configuration: {error}");
                }
            }
        }
        Err(error) => {
            failures += 1;
            println!("[fail] configuration: {error}");
        }
    }

    if failures > 0 {
        bail!("doctor found {failures} issue(s)");
    }

    println!("doctor checks passed");
    Ok(())
}

async fn run_ask_command(repo: &GitRepo, query: &str) -> Result<()> {
    let query = query.trim();
    if query.is_empty() {
        println!("Usage: gg ask <natural language query>");
        println!("Example: gg ask \"undo last commit but keep changes\"");
        return Ok(());
    }

    repo.ensure_git_available()?;
    repo.ensure_repo()?;

    let status = repo.status()?;
    let branch = status
        .branch
        .clone()
        .unwrap_or_else(|| "DETACHED".to_string());
    let recent_log = repo.recent_log(5).unwrap_or_default();

    let context = AskContext {
        branch,
        staged_count: status.staged_count,
        unstaged_count: status.unstaged_count,
        recent_log,
    };

    let config = AiConfig::load()?;
    let client = AiClient::new(config)?;

    let suggestion = client.generate_ask_suggestion(query, &context).await?;

    let risk_levels: Vec<_> = suggestion
        .recommended
        .iter()
        .map(|cmd| risk::classify_risk(&cmd.command))
        .collect();

    let action = tui::run_ask(&suggestion, &risk_levels)?;

    match action {
        AskAction::RunRecommended => {
            execute_ask_commands(repo, &suggestion.recommended).await?;
        }
        AskAction::RunAlternative => {
            if let Some(alt) = &suggestion.alternative {
                execute_ask_commands(repo, alt).await?;
            } else {
                println!("No alternative available.");
            }
        }
        AskAction::Cancel => {
            println!("ask cancelled");
        }
    }

    Ok(())
}

fn is_git_commit_command(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    (trimmed == "git commit" || trimmed.starts_with("git commit "))
        && !trimmed.contains("--amend")
        && !trimmed.contains("--no-edit")
}

async fn execute_ask_commands(repo: &GitRepo, commands: &[SuggestedCommand]) -> Result<()> {
    for (index, cmd) in commands.iter().enumerate() {
        if is_git_commit_command(&cmd.command) {
            run_commit(repo).await?;
            continue;
        }

        let risk = risk::classify_risk(&cmd.command);
        if matches!(risk, risk::RiskLevel::Dangerous) {
            if !tui::confirm_dangerous_command(&cmd.command, &cmd.description)? {
                println!("cancelled before executing: {}", cmd.command);
                let remaining = &commands[index..];
                if !remaining.is_empty() {
                    println!("commands not executed:");
                    for r in remaining {
                        println!("  {}", r.command);
                    }
                }
                return Ok(());
            }
        }

        match repo.run_suggested_command(&cmd.command) {
            Ok(output) => {
                if !output.trim().is_empty() {
                    println!("{output}");
                }
            }
            Err(error) => {
                let remaining = &commands[index + 1..];
                if !remaining.is_empty() {
                    eprintln!("Error: {error}");
                    println!("remaining commands not executed:");
                    for r in remaining {
                        println!("  {}", r.command);
                    }
                }
                return Err(error);
            }
        }
    }
    Ok(())
}

fn is_known_git_subcommand(name: &str) -> bool {
    matches!(
        name,
        "add"
            | "bisect"
            | "blame"
            | "branch"
            | "checkout"
            | "cherry-pick"
            | "clean"
            | "clone"
            | "commit"
            | "config"
            | "describe"
            | "diff"
            | "fetch"
            | "format-patch"
            | "grep"
            | "init"
            | "log"
            | "merge"
            | "mergetool"
            | "mv"
            | "notes"
            | "pull"
            | "push"
            | "rebase"
            | "remote"
            | "reset"
            | "restore"
            | "revert"
            | "rm"
            | "shortlog"
            | "show"
            | "stash"
            | "status"
            | "submodule"
            | "switch"
            | "tag"
            | "worktree"
    )
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
