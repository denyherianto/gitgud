use std::{collections::BTreeMap, env, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};

use crate::config::{
    CommitStyle, GenerationMode, Provider, ResolvedConventionalPreset, resolve_ai_settings,
};

pub const DEFAULT_PROVIDER: Provider = Provider::Gemini;
pub const DEFAULT_BASE_API_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";
pub const DEFAULT_BASE_MODEL: &str = "gemini-2.5-flash";
pub const DEFAULT_COMMIT_STYLE: CommitStyle = CommitStyle::Standard;
const DEFAULT_TIMEOUT_SECS: u64 = 60;
const DEFAULT_MAX_DIFF_CHARS: usize = 12_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiConfig {
    pub api_token: String,
    pub provider: Provider,
    pub base_api_url: String,
    pub base_model: String,
    pub commit_style: CommitStyle,
    pub generation_mode: GenerationMode,
    pub conventional_preset: ResolvedConventionalPreset,
    pub timeout: Duration,
}

impl AiConfig {
    pub fn load() -> Result<Self> {
        let resolved = resolve_ai_settings()?;
        let mut config = Self::new(
            resolved.api_token.value,
            resolved.provider.value,
            &resolved.base_api_url.value,
            &resolved.base_model.value,
            resolved.commit_style.value,
            resolved.generation_mode.value,
            resolved.conventional_preset.value,
        )?;
        if let Some(timeout_secs) = read_timeout_override() {
            config.timeout = Duration::from_secs(timeout_secs);
        }
        Ok(config)
    }

    pub fn new(
        api_token: String,
        provider: Provider,
        base_api_url: &str,
        base_model: &str,
        commit_style: CommitStyle,
        generation_mode: GenerationMode,
        conventional_preset: ResolvedConventionalPreset,
    ) -> Result<Self> {
        if api_token.trim().is_empty() {
            bail!("API_TOKEN cannot be empty");
        }

        if base_model.trim().is_empty() {
            bail!("BASE_MODEL cannot be empty");
        }

        Ok(Self {
            api_token,
            provider,
            base_api_url: normalize_base_api_url(base_api_url)?,
            base_model: base_model.trim().to_string(),
            commit_style,
            generation_mode,
            conventional_preset,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        })
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[derive(Debug, Clone)]
pub struct PromptInput {
    pub branch: String,
    pub staged_files: Vec<String>,
    pub diff_stat: String,
    pub diff: String,
    pub commit_style: CommitStyle,
    pub conventional_preset: ResolvedConventionalPreset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitSuggestions {
    pub options: Vec<String>,
    pub split: Vec<SplitCommitPlan>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffExplanation {
    pub what_changed: Vec<String>,
    pub possible_intent: Vec<String>,
    pub risk_areas: Vec<String>,
    pub test_suggestions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitCommitPlan {
    pub message: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskSuggestion {
    pub recommended: Vec<SuggestedCommand>,
    pub alternative: Option<Vec<SuggestedCommand>>,
    pub explanation: String,
    pub teaching_note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestedCommand {
    pub command: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct AskContext {
    pub branch: String,
    pub staged_count: usize,
    pub unstaged_count: usize,
    pub recent_log: String,
}

#[derive(Debug, Clone)]
pub struct AiClient {
    config: AiConfig,
    http: Client,
}

impl AiClient {
    pub fn new(config: AiConfig) -> Result<Self> {
        let http = Client::builder()
            .timeout(config.timeout)
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self { config, http })
    }

    pub async fn check_reachability(&self) -> Result<u16> {
        let response = self
            .http
            .get(&self.config.base_api_url)
            .send()
            .await
            .context("failed to reach BASE_API_URL")?;

        Ok(response.status().as_u16())
    }

    pub async fn list_models(&self) -> Result<Vec<String>> {
        fetch_model_options(&self.config.base_api_url, &self.config.api_token).await
    }

    pub async fn generate_commit_message(&self, input: &PromptInput) -> Result<String> {
        let mut options = self.generate_commit_options(input).await?;
        options
            .drain(..)
            .next()
            .ok_or_else(|| anyhow!("AI provider returned no commit message options"))
    }

    pub async fn generate_commit_suggestions(
        &self,
        input: &PromptInput,
    ) -> Result<CommitSuggestions> {
        match self.config.generation_mode {
            GenerationMode::Auto => {
                match self.generate_commit_suggestions_from_provider(input).await {
                    Ok(suggestions) => Ok(suggestions),
                    Err(error) if is_timeout_error(&error) => {
                        let mut suggestions = build_heuristic_commit_suggestions(input);
                        suggestions.note =
                            Some("AI timed out. Showing heuristic commit options.".to_string());
                        Ok(suggestions)
                    }
                    Err(error) => Err(error),
                }
            }
            GenerationMode::AiOnly => self.generate_commit_suggestions_from_provider(input).await,
            GenerationMode::HeuristicOnly => {
                let mut suggestions = build_heuristic_commit_suggestions(input);
                suggestions.note = Some("Using heuristic commit options only.".to_string());
                Ok(suggestions)
            }
        }
    }

    pub async fn generate_commit_options(&self, input: &PromptInput) -> Result<Vec<String>> {
        self.generate_commit_suggestions(input)
            .await
            .map(|suggestions| suggestions.options)
    }

    pub async fn generate_diff_explanation(&self, input: &PromptInput) -> Result<DiffExplanation> {
        let content = self
            .request_chat_completion(
                build_diff_explanation_system_prompt(),
                build_diff_explanation_prompt(input),
            )
            .await?;

        parse_diff_explanation(&content)
    }

    pub async fn generate_ask_suggestion(
        &self,
        query: &str,
        context: &AskContext,
    ) -> Result<AskSuggestion> {
        let content = self
            .request_chat_completion(
                build_ask_system_prompt(),
                build_ask_user_prompt(query, context),
            )
            .await?;

        parse_ask_suggestion(&content)
    }

    async fn generate_commit_suggestions_from_provider(
        &self,
        input: &PromptInput,
    ) -> Result<CommitSuggestions> {
        let content = self
            .request_chat_completion(
                build_system_prompt(input.commit_style, &input.conventional_preset),
                build_commit_prompt(input),
            )
            .await?;

        parse_commit_suggestions_with_preset(
            &content,
            input,
            input.commit_style,
            &input.conventional_preset,
        )
    }

    async fn request_chat_completion(
        &self,
        system_prompt: String,
        user_prompt: String,
    ) -> Result<String> {
        let endpoint = format!("{}/chat/completions", self.config.base_api_url);
        let request = ChatCompletionRequest {
            model: self.config.base_model.clone(),
            temperature: 0.2,
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system_prompt,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: user_prompt,
                },
            ],
        };

        let response = self
            .http
            .post(endpoint)
            .bearer_auth(&self.config.api_token)
            .json(&request)
            .send()
            .await
            .context("failed to call AI provider")?;

        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read AI response")?;

        if !status.is_success() {
            bail!("AI provider returned {}: {}", status, body.trim());
        }

        let parsed: ChatCompletionResponse =
            serde_json::from_str(&body).context("failed to parse AI response JSON")?;
        parsed
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .ok_or_else(|| anyhow!("AI provider returned no choices"))
    }
}

pub async fn fetch_model_options(base_api_url: &str, api_token: &str) -> Result<Vec<String>> {
    let api_token = api_token.trim();
    if api_token.is_empty() {
        bail!("API_TOKEN cannot be empty");
    }

    let endpoint = format!("{}/models", normalize_base_api_url(base_api_url)?);
    let client = Client::builder()
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .build()
        .context("failed to build HTTP client")?;

    let response = client
        .get(endpoint)
        .bearer_auth(api_token)
        .send()
        .await
        .context("failed to call AI provider model list endpoint")?;

    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read AI model list response")?;

    if !status.is_success() {
        bail!(
            "AI provider returned {} while listing models: {}",
            status,
            body.trim()
        );
    }

    let parsed: ModelListResponse =
        serde_json::from_str(&body).context("failed to parse AI model list response JSON")?;
    let models = collect_model_options(parsed.data);

    if models.is_empty() {
        bail!("AI provider returned no models");
    }

    Ok(models)
}

fn read_timeout_override() -> Option<u64> {
    let raw = env::var("AI_TIMEOUT_SECS").ok()?;
    let secs: u64 = raw.trim().parse().ok()?;
    if secs == 0 { None } else { Some(secs) }
}

pub fn normalize_base_api_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        bail!("BASE_API_URL cannot be empty");
    }

    let url = Url::parse(trimmed).context("BASE_API_URL must be a valid absolute URL")?;
    Ok(url.to_string().trim_end_matches('/').to_string())
}

pub fn build_commit_prompt(input: &PromptInput) -> String {
    format!(
        "Commit style: {}{}\n{}",
        match input.commit_style {
            CommitStyle::Standard => "standard",
            CommitStyle::Conventional => "conventional",
        },
        build_conventional_context(input),
        build_diff_context(input, DEFAULT_MAX_DIFF_CHARS)
    )
}

pub fn build_diff_explanation_prompt(input: &PromptInput) -> String {
    format!(
        "Explain this staged diff in four sections: what changed, possible intent, risk areas, and test suggestions.\n{}",
        build_diff_context(input, DEFAULT_MAX_DIFF_CHARS)
    )
}

fn build_diff_context(input: &PromptInput, max_diff_chars: usize) -> String {
    let file_list = if input.staged_files.is_empty() {
        "(none)".to_string()
    } else {
        input.staged_files.join(", ")
    };

    format!(
        "Branch: {}\nStaged files: {}\nDiff summary:\n{}\n\nStaged patch:\n{}",
        input.branch,
        file_list,
        if input.diff_stat.trim().is_empty() {
            "(no diff summary)"
        } else {
            input.diff_stat.trim()
        },
        truncate_diff(&input.diff, max_diff_chars)
    )
}

fn build_conventional_context(input: &PromptInput) -> String {
    if matches!(input.commit_style, CommitStyle::Conventional) {
        format!(
            "\nConventional preset: {}\nAllowed types: {}",
            input.conventional_preset.name,
            input.conventional_preset.types.join(", ")
        )
    } else {
        String::new()
    }
}

pub fn truncate_diff(diff: &str, max_chars: usize) -> String {
    if diff.chars().count() <= max_chars {
        return diff.to_string();
    }

    let truncated: String = diff.chars().take(max_chars).collect();
    format!("{truncated}\n\n[diff truncated]")
}

pub fn validate_commit_message(raw: &str, commit_style: CommitStyle) -> Result<String> {
    validate_commit_message_with_preset(
        raw,
        commit_style,
        &ResolvedConventionalPreset::built_in_default(),
    )
}

pub fn validate_commit_message_with_preset(
    raw: &str,
    commit_style: CommitStyle,
    conventional_preset: &ResolvedConventionalPreset,
) -> Result<String> {
    let normalized = raw.replace("\r\n", "\n");
    let lines: Vec<String> = normalized
        .lines()
        .map(str::trim_end)
        .skip_while(|line| line.is_empty())
        .map(str::to_string)
        .collect();

    if lines.is_empty() {
        bail!("AI provider returned an empty commit message");
    }

    if lines.iter().any(|line| line.contains("```")) {
        bail!("commit message must not contain markdown fences");
    }

    let subject = lines[0].trim();
    if subject.is_empty() {
        bail!("commit message subject cannot be empty");
    }

    if subject.chars().count() > 72 {
        bail!("commit message subject exceeds 72 characters");
    }

    if matches!(commit_style, CommitStyle::Conventional) {
        validate_conventional_subject(subject, &conventional_preset.types)?;
    }

    let body_lines = lines[1..]
        .iter()
        .skip_while(|line| line.is_empty())
        .map(|line| line.trim_end().to_string())
        .collect::<Vec<_>>();

    let message = if body_lines.is_empty() {
        subject.to_string()
    } else {
        format!("{subject}\n\n{}", body_lines.join("\n"))
    };

    Ok(message)
}

pub fn build_heuristic_commit_suggestions(input: &PromptInput) -> CommitSuggestions {
    let subject = describe_change_subject(&input.staged_files);
    let descriptions = build_heuristic_option_descriptions(input, &subject);
    let standard = descriptions
        .iter()
        .map(|description| sentence_case(description))
        .collect::<Vec<_>>();

    let conventional_type = infer_conventional_type(input);
    let conventional_scope = infer_conventional_scope(&input.staged_files);
    let conventional = descriptions
        .iter()
        .map(|description| format!("{conventional_type}({conventional_scope}): {description}"))
        .collect::<Vec<_>>();

    let candidates = match input.commit_style {
        CommitStyle::Standard => standard,
        CommitStyle::Conventional => conventional,
    };

    CommitSuggestions {
        options: candidates.into_iter().map(limit_subject_to_72).collect(),
        split: build_heuristic_split_suggestions(input),
        note: None,
    }
}

pub fn build_heuristic_commit_options(input: &PromptInput) -> Vec<String> {
    build_heuristic_commit_suggestions(input).options
}

fn build_system_prompt(
    commit_style: CommitStyle,
    conventional_preset: &ResolvedConventionalPreset,
) -> String {
    let style_rules = match commit_style {
        CommitStyle::Standard => {
            "Use standard commit messages with an imperative subject under 72 characters."
                .to_string()
        }
        CommitStyle::Conventional => format!(
            "Use Conventional Commits. Every subject must match type(scope optional)!: description. Only use these commit types from the '{}' preset: {}.",
            conventional_preset.name,
            conventional_preset.types.join(", ")
        ),
    };

    format!(
        "You write concise Git commit messages. Return valid JSON only with this shape: {{\"options\":[\"message 1\",\"message 2\",\"message 3\"],\"split\":[{{\"message\":\"message a\",\"files\":[\"path/a.rs\"]}},{{\"message\":\"message b\",\"files\":[\"path/b.rs\"]}}]}}. Provide 1 to 3 distinct options in `options`. Add `split` only when the staged changes mix multiple concerns that should be committed separately; when present, `split` must contain 2 to 4 objects and each object must include a commit `message` plus the staged `files` that belong in that commit. The split plan must cover every staged file exactly once. Each message may include a blank line and body, but no markdown fences, labels, numbering, or commentary. Describe only the staged changes. {style_rules}"
    )
}

fn build_diff_explanation_system_prompt() -> String {
    "You explain staged Git diffs for engineers. Return valid JSON only with this shape: {\"what_changed\":[\"item 1\"],\"possible_intent\":[\"item 1\"],\"risk_areas\":[\"item 1\"],\"test_suggestions\":[\"item 1\"]}. Each field must be an array of 1 to 4 concise strings. Base `what_changed`, `risk_areas`, and `test_suggestions` on the diff only. `possible_intent` may be an inference from the diff. Do not use markdown fences, headings, numbering, or extra commentary.".to_string()
}

fn build_ask_system_prompt() -> String {
    r#"You are a Git command assistant. Given a natural language description, suggest the exact git command(s). Return valid JSON with this shape: {"recommended":[{"command":"git ...","description":"..."}],"alternative":[{"command":"git ...","description":"..."}],"explanation":"...","teaching_note":"..."}. `recommended` is 1-4 commands in execution order. `alternative` is optional — use null if there is no meaningful alternative. Every `command` must start with "git ". `explanation` gives context on the recommended approach. `teaching_note` explains the underlying Git concept. No markdown fences."#.to_string()
}

fn build_ask_user_prompt(query: &str, context: &AskContext) -> String {
    let log = if context.recent_log.trim().is_empty() {
        "(none)".to_string()
    } else {
        context.recent_log.clone()
    };
    format!(
        "Query: {}\n\nContext:\nBranch: {}\nStaged files: {}\nUnstaged files: {}\nRecent commits:\n{}",
        query, context.branch, context.staged_count, context.unstaged_count, log
    )
}

fn parse_ask_suggestion(raw: &str) -> Result<AskSuggestion> {
    let json = raw.trim();

    let parsed: AskSuggestionPayload = if let Ok(p) = serde_json::from_str(json) {
        p
    } else if let Some(stripped) = strip_code_fence(json) {
        serde_json::from_str(stripped).context("failed to parse ask suggestion JSON")?
    } else {
        bail!("failed to parse ask suggestion JSON");
    };

    if parsed.recommended.is_empty() {
        bail!("AI provider returned no recommended commands");
    }

    for cmd in &parsed.recommended {
        if !cmd.command.trim_start().starts_with("git ") {
            bail!(
                "AI provider returned a command that does not start with 'git ': {}",
                cmd.command
            );
        }
    }

    if let Some(alt) = &parsed.alternative {
        for cmd in alt {
            if !cmd.command.trim_start().starts_with("git ") {
                bail!(
                    "AI provider returned an alternative command that does not start with 'git ': {}",
                    cmd.command
                );
            }
        }
    }

    Ok(AskSuggestion {
        recommended: parsed
            .recommended
            .into_iter()
            .map(|c| SuggestedCommand {
                command: c.command,
                description: c.description,
            })
            .collect(),
        alternative: parsed.alternative.map(|alts| {
            alts.into_iter()
                .map(|c| SuggestedCommand {
                    command: c.command,
                    description: c.description,
                })
                .collect()
        }),
        explanation: parsed.explanation,
        teaching_note: parsed.teaching_note,
    })
}

#[cfg(test)]
fn parse_commit_suggestions(raw: &str, commit_style: CommitStyle) -> Result<CommitSuggestions> {
    let input = PromptInput {
        branch: "feature/billing".into(),
        staged_files: vec!["src/billing.rs".into(), "src/subscription.rs".into()],
        diff_stat: "2 files changed".into(),
        diff: "diff --git a/src/billing.rs b/src/billing.rs\n+fn billing_summary_card() {}\ndiff --git a/src/subscription.rs b/src/subscription.rs\n+if status == null {\n+    return;\n+}\n".into(),
        commit_style,
        conventional_preset: ResolvedConventionalPreset::built_in_default(),
    };

    parse_commit_suggestions_with_preset(
        raw,
        &input,
        commit_style,
        &ResolvedConventionalPreset::built_in_default(),
    )
}

fn parse_commit_suggestions_with_preset(
    raw: &str,
    input: &PromptInput,
    commit_style: CommitStyle,
    conventional_preset: &ResolvedConventionalPreset,
) -> Result<CommitSuggestions> {
    let parsed = parse_options_payload(raw)?;
    if !(1..=3).contains(&parsed.options.len()) {
        bail!("AI provider must return between 1 and 3 commit message options");
    }

    let mut options = Vec::with_capacity(parsed.options.len());
    for option in parsed.options {
        options.push(normalize_generated_commit_message_with_preset(
            &option,
            commit_style,
            conventional_preset,
        )?);
    }

    if !parsed.split.is_empty() && !(2..=4).contains(&parsed.split.len()) {
        bail!("AI provider split suggestions must contain between 2 and 4 messages");
    }

    let split = resolve_split_plans(parsed.split, input, commit_style, conventional_preset)?;

    Ok(CommitSuggestions {
        options,
        split,
        note: None,
    })
}

fn parse_diff_explanation(raw: &str) -> Result<DiffExplanation> {
    let parsed: DiffExplanationPayload =
        serde_json::from_str(raw).context("failed to parse diff explanation JSON")?;

    Ok(DiffExplanation {
        what_changed: normalize_explanation_items(parsed.what_changed, "what_changed")?,
        possible_intent: normalize_explanation_items(parsed.possible_intent, "possible_intent")?,
        risk_areas: normalize_explanation_items(parsed.risk_areas, "risk_areas")?,
        test_suggestions: normalize_explanation_items(parsed.test_suggestions, "test_suggestions")?,
    })
}

fn normalize_explanation_items(items: Vec<String>, field: &str) -> Result<Vec<String>> {
    let normalized = items
        .into_iter()
        .map(|item| item.trim().trim_start_matches("- ").trim().to_string())
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>();

    if normalized.is_empty() {
        bail!("AI provider diff explanation field `{field}` must contain at least one item");
    }

    if normalized.iter().any(|item| item.contains("```")) {
        bail!("diff explanation field `{field}` must not contain markdown fences");
    }

    Ok(normalized)
}

#[cfg(test)]
fn parse_commit_options(raw: &str, commit_style: CommitStyle) -> Result<Vec<String>> {
    parse_commit_suggestions_with_preset(
        raw,
        &PromptInput {
            branch: "main".into(),
            staged_files: vec!["src/main.rs".into()],
            diff_stat: String::new(),
            diff: String::new(),
            commit_style,
            conventional_preset: ResolvedConventionalPreset::built_in_default(),
        },
        commit_style,
        &ResolvedConventionalPreset::built_in_default(),
    )
    .map(|parsed| parsed.options)
}

fn resolve_split_plans(
    split: Vec<SplitPlanPayload>,
    input: &PromptInput,
    commit_style: CommitStyle,
    conventional_preset: &ResolvedConventionalPreset,
) -> Result<Vec<SplitCommitPlan>> {
    if split.is_empty() {
        return Ok(Vec::new());
    }

    let heuristic = build_heuristic_split_suggestions(input);
    let all_structured = split
        .iter()
        .all(|item| matches!(item, SplitPlanPayload::Plan { .. }));
    if all_structured {
        let provider =
            validate_provider_split_plans(split, input, commit_style, conventional_preset)?;
        return Ok(provider);
    }

    if heuristic.len() == split.len() {
        let mut plans = Vec::with_capacity(split.len());
        for (item, heuristic_plan) in split.into_iter().zip(heuristic) {
            let message = match item {
                SplitPlanPayload::Message(message) => message,
                SplitPlanPayload::Plan { message, .. } => message,
            };
            plans.push(SplitCommitPlan {
                message: normalize_generated_commit_message_with_preset(
                    &message,
                    commit_style,
                    conventional_preset,
                )?,
                files: heuristic_plan.files,
            });
        }
        return Ok(plans);
    }

    Ok(heuristic)
}

fn validate_provider_split_plans(
    split: Vec<SplitPlanPayload>,
    input: &PromptInput,
    commit_style: CommitStyle,
    conventional_preset: &ResolvedConventionalPreset,
) -> Result<Vec<SplitCommitPlan>> {
    let mut plans = Vec::with_capacity(split.len());
    let mut seen_files = BTreeMap::new();
    let staged_files = input
        .staged_files
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();

    for item in split {
        let SplitPlanPayload::Plan { message, files } = item else {
            bail!("AI provider split suggestions must include files");
        };
        if files.is_empty() {
            bail!("AI provider split suggestions must include at least one file per commit");
        }

        let mut unique_files = Vec::with_capacity(files.len());
        for file in files {
            if !staged_files.iter().any(|staged| *staged == file) {
                bail!("AI provider split suggestions referenced an unstaged file: {file}");
            }
            if seen_files.insert(file.clone(), message.clone()).is_none() {
                unique_files.push(file);
            } else {
                bail!("AI provider split suggestions assigned a file to multiple commits");
            }
        }

        plans.push(SplitCommitPlan {
            message: normalize_generated_commit_message_with_preset(
                &message,
                commit_style,
                conventional_preset,
            )?,
            files: unique_files,
        });
    }

    if input
        .staged_files
        .iter()
        .any(|file| !seen_files.contains_key(file))
    {
        bail!("AI provider split suggestions must cover every staged file");
    }

    Ok(plans)
}

fn parse_options_payload(raw: &str) -> Result<CommitOptionsPayload> {
    let json = raw.trim();

    if let Ok(parsed) = serde_json::from_str::<CommitOptionsPayload>(json) {
        return Ok(parsed);
    }

    if let Some(stripped) = strip_code_fence(json) {
        return serde_json::from_str::<CommitOptionsPayload>(stripped)
            .context("failed to parse commit options JSON");
    }

    Err(anyhow!("failed to parse commit options JSON"))
}

fn strip_code_fence(raw: &str) -> Option<&str> {
    let stripped = raw.strip_prefix("```")?;
    let stripped = stripped
        .strip_prefix("json")
        .or_else(|| stripped.strip_prefix("JSON"))
        .unwrap_or(stripped);
    let stripped = stripped.strip_prefix('\n').unwrap_or(stripped);
    stripped.strip_suffix("```").map(str::trim)
}

fn validate_conventional_subject(subject: &str, allowed_types: &[String]) -> Result<()> {
    let (head, description) = subject
        .split_once(": ")
        .ok_or_else(|| anyhow!("conventional commit subject must contain ': '"))?;
    if description.trim().is_empty() {
        bail!("conventional commit description cannot be empty");
    }

    let head = head.strip_suffix('!').unwrap_or(head);
    let (commit_type, scope) = if let Some((commit_type, remainder)) = head.split_once('(') {
        let scope = remainder
            .strip_suffix(')')
            .ok_or_else(|| anyhow!("conventional commit scope must end with ')'"))?;
        (commit_type, Some(scope))
    } else {
        (head, None)
    };

    if commit_type.is_empty()
        || !commit_type
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch == '-')
    {
        bail!("conventional commit type must use lowercase ASCII letters or hyphens");
    }

    if !allowed_types.iter().any(|allowed| allowed == commit_type) {
        bail!(
            "conventional commit type must be one of: {}",
            allowed_types.join(", ")
        );
    }

    if let Some(scope) = scope {
        if scope.is_empty()
            || !scope
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '/' || ch == '_')
        {
            bail!("conventional commit scope contains unsupported characters");
        }
    }

    Ok(())
}

fn normalize_generated_commit_message_with_preset(
    raw: &str,
    commit_style: CommitStyle,
    conventional_preset: &ResolvedConventionalPreset,
) -> Result<String> {
    let normalized = raw.replace("\r\n", "\n");
    let lines: Vec<String> = normalized
        .lines()
        .map(str::trim_end)
        .skip_while(|line| line.is_empty())
        .map(str::to_string)
        .collect();

    let Some((subject, body)) = lines.split_first() else {
        return validate_commit_message_with_preset(raw, commit_style, conventional_preset);
    };

    let subject = shorten_generated_subject(subject, commit_style);
    let candidate = if body.is_empty() {
        subject
    } else {
        format!("{subject}\n{}", body.join("\n"))
    };

    validate_commit_message_with_preset(&candidate, commit_style, conventional_preset)
}

fn shorten_generated_subject(subject: &str, commit_style: CommitStyle) -> String {
    if subject.chars().count() <= 72 {
        return subject.to_string();
    }

    if matches!(commit_style, CommitStyle::Conventional) {
        if let Some((head, description)) = subject.split_once(": ") {
            let available = 72usize.saturating_sub(head.chars().count() + 2);
            if available > 0 {
                let shortened_description = shorten_text_to_limit(description, available);
                if !shortened_description.is_empty() {
                    return format!("{head}: {shortened_description}");
                }
            }
        }
    }

    shorten_text_to_limit(subject, 72)
}

fn shorten_text_to_limit(raw: &str, limit: usize) -> String {
    if raw.chars().count() <= limit {
        return raw.to_string();
    }

    let truncated = raw.chars().take(limit).collect::<String>();
    let boundary = truncated
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace() || *ch == '-' || *ch == ',' || *ch == ';')
        .map(|(index, _)| index);

    boundary
        .and_then(|index| {
            let candidate = truncated[..index]
                .trim_end_matches([' ', '-', ',', ';', ':'])
                .trim_end();
            (!candidate.is_empty()).then(|| candidate.to_string())
        })
        .unwrap_or_else(|| truncated.trim_end().to_string())
}

fn is_timeout_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_timeout)
    })
}

fn build_heuristic_split_suggestions(input: &PromptInput) -> Vec<SplitCommitPlan> {
    let concerns = collect_concerns(input);
    if concerns.len() < 2 {
        return Vec::new();
    }

    concerns
        .into_iter()
        .take(4)
        .map(|concern| {
            let message = limit_subject_to_72(build_split_message(
                &concern,
                input.commit_style,
                &input.conventional_preset.types,
            ));
            SplitCommitPlan {
                message,
                files: concern.files,
            }
        })
        .collect()
}

fn build_split_message(
    concern: &Concern,
    commit_style: CommitStyle,
    conventional_types: &[String],
) -> String {
    let is_fix = concern.diff_mentions_fix || concern.label.contains("fix");
    let is_feature = concern.diff_mentions_feature;

    match commit_style {
        CommitStyle::Standard => {
            if concern.kind == ConcernKind::Docs {
                format!("Update {}", concern.label)
            } else if concern.kind == ConcernKind::Tests {
                format!("Expand {} coverage", concern.label)
            } else if concern.kind == ConcernKind::Ci {
                format!("Update {}", concern.label)
            } else if is_fix {
                format!("Fix {} handling", concern.label)
            } else if is_feature {
                format!("Add {}", concern.label)
            } else {
                format!("Update {}", concern.label)
            }
        }
        CommitStyle::Conventional => {
            let preferred_types: &[&str] = match concern.kind {
                ConcernKind::Docs => &["docs", "chore"],
                ConcernKind::Tests => &["test", "chore"],
                ConcernKind::Ci => &["ci", "build", "chore"],
                ConcernKind::Install => &["build", "chore"],
                ConcernKind::Other => &["chore", "build"],
                ConcernKind::Source if is_fix => &["fix", "refactor", "chore"],
                ConcernKind::Source if is_feature => &["feat", "refactor", "chore"],
                ConcernKind::Source => &["feat", "refactor", "chore"],
            };
            let commit_type = pick_conventional_type(conventional_types, preferred_types);
            let scope = sanitize_scope(&concern.scope);
            let description = match concern.kind {
                ConcernKind::Docs => format!("update {}", concern.label),
                ConcernKind::Tests => format!("expand {} coverage", concern.label),
                ConcernKind::Ci | ConcernKind::Install | ConcernKind::Other => {
                    format!("update {}", concern.label)
                }
                ConcernKind::Source if is_fix => format!("handle {}", concern.label),
                ConcernKind::Source if is_feature => format!("add {}", concern.label),
                ConcernKind::Source => format!("update {}", concern.label),
            };

            if scope.is_empty() {
                format!("{commit_type}: {description}")
            } else {
                format!("{commit_type}({scope}): {description}")
            }
        }
    }
}

fn collect_concerns(input: &PromptInput) -> Vec<Concern> {
    let diffs_by_path = split_diff_by_path(&input.diff);
    let mut concerns: Vec<Concern> = Vec::new();

    for path in &input.staged_files {
        let Some(mut concern) = classify_concern(path) else {
            continue;
        };

        if let Some(diff) = diffs_by_path.get(path) {
            let diff_lower = diff.to_ascii_lowercase();
            concern.diff_mentions_fix = contains_any(
                &diff_lower,
                &[
                    "null", "none", "missing", "fallback", "guard", "error", "handle",
                ],
            );
            concern.diff_mentions_feature = contains_any(
                &diff_lower,
                &[
                    "add ", "new ", "create", "card", "screen", "page", "summary",
                ],
            );
        }

        if let Some(existing) = concerns.iter_mut().find(|item| {
            let item = &**item;
            item.kind == concern.kind && item.scope == concern.scope && item.label == concern.label
        }) {
            existing.diff_mentions_fix |= concern.diff_mentions_fix;
            existing.diff_mentions_feature |= concern.diff_mentions_feature;
            existing.files.push(path.clone());
        } else {
            concern.files.push(path.clone());
            concerns.push(concern);
        }
    }

    concerns
}

fn classify_concern(path: &str) -> Option<Concern> {
    if path == "README.md" || path.starts_with("docs/") {
        return Some(Concern::new(ConcernKind::Docs, "docs", "documentation"));
    }

    if path.starts_with("tests/") {
        let label = path
            .strip_prefix("tests/")
            .and_then(|rest| rest.split('/').next())
            .and_then(|segment| segment.split('.').next())
            .map(humanize_identifier)
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| "tests".to_string());
        return Some(Concern::new(ConcernKind::Tests, "tests", &label));
    }

    if path.starts_with(".github/workflows/") {
        return Some(Concern::new(ConcernKind::Ci, "release", "release workflow"));
    }

    if path == "install.sh" {
        return Some(Concern::new(ConcernKind::Install, "install", "installer"));
    }

    if let Some(rest) = path.strip_prefix("src/") {
        let module = rest
            .split('/')
            .next()
            .and_then(|segment| segment.split('.').next())
            .filter(|segment| !segment.is_empty())
            .unwrap_or("project");
        let label = humanize_identifier(module);
        return Some(Concern::new(ConcernKind::Source, module, &label));
    }

    let top_level = path
        .split('/')
        .next()
        .and_then(|segment| segment.split('.').next())
        .filter(|segment| !segment.is_empty())?;
    let label = humanize_identifier(top_level);
    Some(Concern::new(ConcernKind::Other, top_level, &label))
}

fn split_diff_by_path(diff: &str) -> BTreeMap<String, String> {
    let mut segments = BTreeMap::new();
    let mut current_path: Option<String> = None;
    let mut current_lines = String::new();

    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git a/") {
            if let Some(path) = current_path.take() {
                segments.insert(path, current_lines.trim().to_string());
                current_lines.clear();
            }

            current_path = rest
                .split_once(" b/")
                .map(|(_, path)| path.to_string())
                .or_else(|| rest.split_whitespace().nth(1).map(str::to_string));
        } else if current_path.is_some() {
            current_lines.push_str(line);
            current_lines.push('\n');
        }
    }

    if let Some(path) = current_path {
        segments.insert(path, current_lines.trim().to_string());
    }

    segments
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn describe_change_subject(staged_files: &[String]) -> String {
    if staged_files
        .iter()
        .any(|path| path == ".github/workflows/release.yml")
    {
        return "release workflow".to_string();
    }

    if staged_files
        .iter()
        .all(|path| path == "README.md" || path.starts_with("docs/"))
    {
        return "documentation".to_string();
    }

    if staged_files.iter().all(|path| path.starts_with("tests/")) {
        return "test coverage".to_string();
    }

    if staged_files.iter().any(|path| path == "install.sh") {
        return "installer".to_string();
    }

    if let Some(module) = staged_files
        .iter()
        .filter_map(|path| path.strip_prefix("src/"))
        .filter_map(|path| path.strip_suffix(".rs"))
        .next()
    {
        return humanize_identifier(module);
    }

    if staged_files.len() == 1 {
        return staged_files
            .first()
            .map(|path| {
                path.rsplit('/')
                    .next()
                    .and_then(|file| file.split('.').next())
                    .map(humanize_identifier)
                    .unwrap_or_else(|| "project".to_string())
            })
            .unwrap_or_else(|| "project".to_string());
    }

    staged_files
        .iter()
        .filter_map(|path| path.split('/').next())
        .find(|segment| !segment.is_empty() && *segment != ".")
        .map(humanize_identifier)
        .unwrap_or_else(|| "project".to_string())
}

fn build_heuristic_option_descriptions(input: &PromptInput, subject: &str) -> [String; 3] {
    let diff = staged_patch_signal_text(&input.diff);

    if input
        .staged_files
        .iter()
        .all(|path| path == "README.md" || path.starts_with("docs/"))
    {
        return [
            format!("update {subject}"),
            format!("clarify {subject} details"),
            format!("revise {subject} guidance"),
        ];
    }

    if input
        .staged_files
        .iter()
        .all(|path| path.starts_with("tests/"))
    {
        return [
            format!("expand {subject} coverage"),
            format!("add {subject} regression tests"),
            format!("strengthen {subject} assertions"),
        ];
    }

    if input
        .staged_files
        .iter()
        .any(|path| path.starts_with(".github/workflows/"))
    {
        return [
            format!("update {subject}"),
            format!("improve {subject} reliability"),
            format!("refine {subject} checks"),
        ];
    }

    if input
        .staged_files
        .iter()
        .any(|path| path == "install.sh" || path == "Cargo.toml" || path == "Cargo.lock")
    {
        return [
            format!("update {subject} setup"),
            format!("improve {subject} reliability"),
            format!("streamline {subject} flow"),
        ];
    }

    if contains_any(&diff, &["retry"]) && contains_any(&diff, &["fallback"]) {
        return [
            format!("retry {subject} generation before fallback"),
            format!("reduce {subject} fallback usage"),
            format!("improve {subject} timeout recovery"),
        ];
    }

    if contains_any(&diff, &["timeout", "timed out"]) {
        return [
            format!("handle {subject} timeouts gracefully"),
            format!("retry {subject} requests on timeout"),
            format!("improve {subject} timeout recovery"),
        ];
    }

    if contains_any(&diff, &["fallback"]) {
        return [
            format!("reduce {subject} fallback usage"),
            format!("tighten {subject} fallback handling"),
            format!("improve {subject} reliability"),
        ];
    }

    if contains_any(
        &diff,
        &[
            "perf",
            "optimiz",
            "faster",
            "latency",
            "cache",
            "throughput",
        ],
    ) {
        return [
            format!("optimize {subject}"),
            format!("reduce {subject} latency"),
            format!("improve {subject} throughput"),
        ];
    }

    if contains_any(
        &diff,
        &["null", "none", "missing", "guard", "error", "handle", "fix"],
    ) {
        return [
            format!("fix {subject} handling"),
            format!("guard {subject} edge cases"),
            format!("improve {subject} reliability"),
        ];
    }

    if contains_any(
        &diff,
        &["add ", "new ", "create", "introduce", "support", "enable"],
    ) {
        return [
            format!("add {subject} support"),
            format!("expand {subject} flow"),
            format!("introduce {subject} improvements"),
        ];
    }

    [
        format!("improve {subject}"),
        format!("refine {subject} behavior"),
        format!("clean up {subject} flow"),
    ]
}

fn staged_patch_signal_text(diff: &str) -> String {
    diff.lines()
        .filter_map(|line| {
            if let Some(added) = line.strip_prefix('+') {
                return (!line.starts_with("+++")).then_some(added);
            }

            if let Some(removed) = line.strip_prefix('-') {
                return (!line.starts_with("---")).then_some(removed);
            }

            None
        })
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase()
}

fn infer_conventional_type(input: &PromptInput) -> String {
    let branch = input.branch.to_ascii_lowercase();
    let diff = input.diff.to_ascii_lowercase();
    let preferred: &[&str] = if input
        .staged_files
        .iter()
        .any(|path| path.starts_with(".github/"))
    {
        &["ci", "build", "chore"]
    } else if input
        .staged_files
        .iter()
        .all(|path| path == "README.md" || path.starts_with("docs/"))
    {
        &["docs", "chore"]
    } else if input
        .staged_files
        .iter()
        .all(|path| path.starts_with("tests/"))
    {
        &["test", "chore"]
    } else if input
        .staged_files
        .iter()
        .any(|path| path == "install.sh" || path == "Cargo.toml" || path == "Cargo.lock")
    {
        &["build", "chore", "ci"]
    } else if contains_any(
        &diff,
        &[
            "perf",
            "optimiz",
            "faster",
            "latency",
            "cache",
            "throughput",
        ],
    ) {
        &["perf", "refactor", "feat", "chore"]
    } else if contains_any(
        &diff,
        &["refactor", "rename", "extract", "cleanup", "restructure"],
    ) {
        &["refactor", "feat", "chore"]
    } else if branch.starts_with("fix/")
        || branch.starts_with("bugfix/")
        || branch.starts_with("hotfix/")
    {
        &["fix", "refactor", "chore"]
    } else if branch.starts_with("feat/") || branch.starts_with("feature/") {
        &["feat", "refactor", "chore"]
    } else {
        &["chore", "feat", "refactor", "fix"]
    };

    pick_conventional_type(&input.conventional_preset.types, preferred)
}

fn pick_conventional_type(conventional_types: &[String], preferred: &[&str]) -> String {
    for commit_type in preferred {
        if conventional_types
            .iter()
            .any(|allowed| allowed == commit_type)
        {
            return (*commit_type).to_string();
        }
    }

    conventional_types
        .first()
        .cloned()
        .unwrap_or_else(|| "chore".to_string())
}

fn infer_conventional_scope(staged_files: &[String]) -> String {
    if staged_files
        .iter()
        .any(|path| path.starts_with(".github/workflows/"))
    {
        return "release".to_string();
    }

    if staged_files.iter().all(|path| path.starts_with("tests/")) {
        return "tests".to_string();
    }

    if staged_files
        .iter()
        .all(|path| path == "README.md" || path.starts_with("docs/"))
    {
        return "docs".to_string();
    }

    if let Some(module) = staged_files
        .iter()
        .filter_map(|path| path.strip_prefix("src/"))
        .filter_map(|path| path.strip_suffix(".rs"))
        .next()
    {
        let scope = sanitize_scope(module);
        if !scope.is_empty() {
            return scope;
        }
    }

    if staged_files.iter().any(|path| path == "install.sh") {
        return "install".to_string();
    }

    "project".to_string()
}

fn humanize_identifier(raw: &str) -> String {
    raw.split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn sanitize_scope(raw: &str) -> String {
    raw.chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '/' || *ch == '_')
        .collect::<String>()
        .to_ascii_lowercase()
}

fn limit_subject_to_72(subject: String) -> String {
    let count = subject.chars().count();
    if count <= 72 {
        return subject;
    }

    subject
        .chars()
        .take(72)
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn sentence_case(raw: &str) -> String {
    let mut chars = raw.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };

    let mut result = first.to_ascii_uppercase().to_string();
    result.push_str(chars.as_str());
    result
}

fn collect_model_options(payloads: Vec<ModelPayload>) -> Vec<String> {
    let mut payloads = payloads.into_iter().enumerate().collect::<Vec<_>>();
    payloads.sort_by(|(left_index, left), (right_index, right)| {
        right
            .created
            .cmp(&left.created)
            .then_with(|| left_index.cmp(right_index))
    });

    let mut models = Vec::new();
    for (_, model) in payloads {
        let id = model.id.trim();
        if !id.is_empty() && !models.iter().any(|existing| existing == id) {
            models.push(id.to_string());
        }
    }

    models
}

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    temperature: f32,
    messages: Vec<ChatMessage>,
}

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct CommitOptionsPayload {
    options: Vec<String>,
    #[serde(default)]
    split: Vec<SplitPlanPayload>,
}

#[derive(Debug, Deserialize)]
struct DiffExplanationPayload {
    what_changed: Vec<String>,
    possible_intent: Vec<String>,
    risk_areas: Vec<String>,
    test_suggestions: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AskSuggestionPayload {
    recommended: Vec<SuggestedCommandPayload>,
    #[serde(default)]
    alternative: Option<Vec<SuggestedCommandPayload>>,
    explanation: String,
    teaching_note: String,
}

#[derive(Debug, Deserialize)]
struct SuggestedCommandPayload {
    command: String,
    description: String,
}

#[derive(Debug, Deserialize)]
struct ModelListResponse {
    #[serde(default)]
    data: Vec<ModelPayload>,
}

#[derive(Debug, Deserialize)]
struct ModelPayload {
    id: String,
    #[serde(default)]
    created: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SplitPlanPayload {
    Message(String),
    Plan { message: String, files: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Concern {
    kind: ConcernKind,
    scope: String,
    label: String,
    diff_mentions_fix: bool,
    diff_mentions_feature: bool,
    files: Vec<String>,
}

impl Concern {
    fn new(kind: ConcernKind, scope: &str, label: &str) -> Self {
        Self {
            kind,
            scope: scope.to_string(),
            label: label.to_string(),
            diff_mentions_fix: false,
            diff_mentions_feature: false,
            files: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConcernKind {
    Docs,
    Tests,
    Ci,
    Install,
    Source,
    Other,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        AiConfig, AskContext, AskSuggestion, DEFAULT_BASE_API_URL, DiffExplanation, ModelPayload,
        SplitCommitPlan, SuggestedCommand, build_ask_user_prompt, build_commit_prompt,
        build_diff_explanation_prompt, build_heuristic_commit_options,
        build_heuristic_commit_suggestions, collect_model_options, normalize_base_api_url,
        parse_ask_suggestion, parse_commit_options, parse_commit_suggestions,
        parse_commit_suggestions_with_preset, parse_diff_explanation, truncate_diff,
        validate_commit_message, validate_commit_message_with_preset,
    };
    use crate::config::{CommitStyle, GenerationMode, Provider, ResolvedConventionalPreset};

    fn default_preset() -> ResolvedConventionalPreset {
        ResolvedConventionalPreset::built_in_default()
    }

    #[test]
    fn normalizes_base_url() {
        let actual = normalize_base_api_url("https://example.com/v1/").unwrap();
        assert_eq!(actual, "https://example.com/v1");
    }

    #[test]
    fn rejects_empty_model() {
        let error = AiConfig::new(
            "token".into(),
            Provider::Gemini,
            DEFAULT_BASE_API_URL,
            "   ",
            CommitStyle::Standard,
            GenerationMode::Auto,
            default_preset(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("BASE_MODEL"));
    }

    #[test]
    fn trims_body_spacing() {
        let actual =
            validate_commit_message("Add TUI flow\n\nRefine navigation\n", CommitStyle::Standard)
                .unwrap();
        assert_eq!(actual, "Add TUI flow\n\nRefine navigation");
    }

    #[test]
    fn rejects_long_subject() {
        let subject = "a".repeat(73);
        let error = validate_commit_message(&subject, CommitStyle::Standard).unwrap_err();
        assert!(error.to_string().contains("72"));
    }

    #[test]
    fn truncates_large_diffs() {
        let diff = "x".repeat(20);
        let actual = truncate_diff(&diff, 10);
        assert!(actual.contains("[diff truncated]"));
        assert!(actual.starts_with("xxxxxxxxxx"));
    }

    #[test]
    fn builds_prompt_with_context() {
        let prompt = build_commit_prompt(&super::PromptInput {
            branch: "main".into(),
            staged_files: vec!["src/main.rs".into()],
            diff_stat: "1 file changed".into(),
            diff: "diff --git".into(),
            commit_style: CommitStyle::Standard,
            conventional_preset: default_preset(),
        });

        assert!(prompt.contains("Branch: main"));
        assert!(prompt.contains("Commit style: standard"));
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("1 file changed"));
    }

    #[test]
    fn includes_conventional_preset_in_prompt() {
        let prompt = build_commit_prompt(&super::PromptInput {
            branch: "main".into(),
            staged_files: vec!["src/main.rs".into()],
            diff_stat: "1 file changed".into(),
            diff: "diff --git".into(),
            commit_style: CommitStyle::Conventional,
            conventional_preset: default_preset(),
        });

        assert!(prompt.contains("Conventional preset: default"));
        assert!(prompt.contains("Allowed types: feat, fix"));
    }

    #[test]
    fn includes_requested_sections_in_diff_explanation_prompt() {
        let prompt = build_diff_explanation_prompt(&super::PromptInput {
            branch: "main".into(),
            staged_files: vec!["src/main.rs".into()],
            diff_stat: "1 file changed".into(),
            diff: "diff --git".into(),
            commit_style: CommitStyle::Standard,
            conventional_preset: default_preset(),
        });

        assert!(prompt.contains("what changed"));
        assert!(prompt.contains("possible intent"));
        assert!(prompt.contains("risk areas"));
        assert!(prompt.contains("test suggestions"));
    }

    #[test]
    fn supports_test_timeout_override() {
        let config = AiConfig::new(
            "token".into(),
            Provider::Gemini,
            DEFAULT_BASE_API_URL,
            "model",
            CommitStyle::Standard,
            GenerationMode::Auto,
            default_preset(),
        )
        .unwrap()
        .with_timeout(Duration::from_secs(1));
        assert_eq!(config.timeout, Duration::from_secs(1));
    }

    #[test]
    fn sorts_models_newest_first_and_deduplicates_ids() {
        let models = collect_model_options(vec![
            ModelPayload {
                id: "gpt-4.1-mini".to_string(),
                created: Some(10),
            },
            ModelPayload {
                id: "gpt-4.1".to_string(),
                created: Some(20),
            },
            ModelPayload {
                id: "gpt-4.1-mini".to_string(),
                created: Some(10),
            },
            ModelPayload {
                id: "gpt-4o".to_string(),
                created: Some(30),
            },
        ]);

        assert_eq!(models, vec!["gpt-4o", "gpt-4.1", "gpt-4.1-mini"]);
    }

    #[test]
    fn parses_commit_options_json() {
        let options = parse_commit_options(
            r#"{"options":["Add TUI flow","Improve push logic","Refine config screen"]}"#,
            CommitStyle::Standard,
        )
        .unwrap();

        assert_eq!(options.len(), 3);
    }

    #[test]
    fn trims_long_generated_standard_subjects() {
        let options = parse_commit_options(
            r#"{"options":["Add support for configurable conventional commit presets in the interactive setup flow"]}"#,
            CommitStyle::Standard,
        )
        .unwrap();

        assert_eq!(
            options[0],
            "Add support for configurable conventional commit presets in the"
        );
        assert!(options[0].chars().count() <= 72);
    }

    #[test]
    fn trims_long_generated_conventional_subjects() {
        let options = parse_commit_options(
            r#"{"options":["feat(config): add support for configurable conventional commit presets in setup flow"]}"#,
            CommitStyle::Conventional,
        )
        .unwrap();

        assert_eq!(
            options[0],
            "feat(config): add support for configurable conventional commit presets"
        );
        assert!(options[0].chars().count() <= 72);
    }

    #[test]
    fn parses_split_commit_suggestions_json() {
        let suggestions = parse_commit_suggestions(
            r#"{"options":["Update billing flow"],"split":["Add billing summary card","Fix subscription status handling"]}"#,
            CommitStyle::Standard,
        )
        .unwrap();

        assert_eq!(suggestions.options.len(), 1);
        assert_eq!(suggestions.split.len(), 2);
        assert_eq!(suggestions.split[0].message, "Add billing summary card");
        assert_eq!(
            suggestions.split[0].files,
            vec!["src/billing.rs".to_string()]
        );
    }

    #[test]
    fn parses_diff_explanation_json() {
        let explanation = parse_diff_explanation(
            r#"{"what_changed":["Adds a new explain command"],"possible_intent":["Help users understand staged diffs before committing"],"risk_areas":["Prompt output could become too verbose"],"test_suggestions":["Cover the AI parser with a mocked response"]}"#,
        )
        .unwrap();

        assert_eq!(
            explanation,
            DiffExplanation {
                what_changed: vec!["Adds a new explain command".to_string()],
                possible_intent: vec![
                    "Help users understand staged diffs before committing".to_string()
                ],
                risk_areas: vec!["Prompt output could become too verbose".to_string()],
                test_suggestions: vec!["Cover the AI parser with a mocked response".to_string()],
            }
        );
    }

    #[test]
    fn trims_long_generated_split_messages() {
        let input = super::PromptInput {
            branch: "main".into(),
            staged_files: vec!["src/main.rs".into(), "src/lib.rs".into()],
            diff_stat: "2 files changed".into(),
            diff: "diff --git a/src/main.rs b/src/main.rs\ndiff --git a/src/lib.rs b/src/lib.rs"
                .into(),
            commit_style: CommitStyle::Conventional,
            conventional_preset: default_preset(),
        };
        let preset = default_preset();
        let suggestions = parse_commit_suggestions_with_preset(
            r#"{"options":["feat(billing): update billing flow"],"split":[{"message":"feat(billing): add billing summary card and improve subscription status visibility","files":["src/main.rs"]},{"message":"fix(billing): handle missing customer payment profile details safely","files":["src/lib.rs"]}]}"#,
            &input,
            CommitStyle::Conventional,
            &preset,
        )
        .unwrap();

        assert_eq!(suggestions.split.len(), 2);
        assert!(
            suggestions
                .split
                .iter()
                .all(|plan| plan.message.chars().count() <= 72)
        );
    }

    #[test]
    fn rejects_split_commit_suggestions_with_invalid_count() {
        let error = parse_commit_suggestions(
            r#"{"options":["Update billing flow"],"split":["Add billing summary card"]}"#,
            CommitStyle::Standard,
        )
        .unwrap_err();

        assert!(error.to_string().contains("between 2 and 4"));
    }

    #[test]
    fn validates_conventional_commit_subjects() {
        let message = validate_commit_message(
            "feat(cli): add multiple commit message options",
            CommitStyle::Conventional,
        )
        .unwrap();

        assert_eq!(message, "feat(cli): add multiple commit message options");
    }

    #[test]
    fn rejects_conventional_types_outside_active_preset() {
        let preset = ResolvedConventionalPreset {
            name: "team".into(),
            types: vec!["feature".into(), "bugfix".into()],
        };
        let error = validate_commit_message_with_preset(
            "feat(cli): add commit message options",
            CommitStyle::Conventional,
            &preset,
        )
        .unwrap_err();

        assert!(error.to_string().contains("feature, bugfix"));
    }

    #[test]
    fn builds_standard_heuristic_commit_options() {
        let options = build_heuristic_commit_options(&super::PromptInput {
            branch: "main".into(),
            staged_files: vec![".github/workflows/release.yml".into()],
            diff_stat: "1 file changed".into(),
            diff: "diff --git".into(),
            commit_style: CommitStyle::Standard,
            conventional_preset: default_preset(),
        });

        assert_eq!(options[0], "Update release workflow");
        assert_eq!(options.len(), 3);
    }

    #[test]
    fn builds_contextual_timeout_fallback_options() {
        let options = build_heuristic_commit_options(&super::PromptInput {
            branch: "main".into(),
            staged_files: vec!["src/ai.rs".into()],
            diff_stat: "1 file changed".into(),
            diff: "diff --git a/src/ai.rs b/src/ai.rs\n+retry request before fallback\n+handle timeout explicitly".into(),
            commit_style: CommitStyle::Standard,
            conventional_preset: default_preset(),
        });

        assert_eq!(options[0], "Retry ai generation before fallback");
        assert_eq!(options[1], "Reduce ai fallback usage");
        assert_eq!(options[2], "Improve ai timeout recovery");
    }

    #[test]
    fn ignores_diff_headers_when_building_fallback_options() {
        let options = build_heuristic_commit_options(&super::PromptInput {
            branch: "main".into(),
            staged_files: vec!["src/fallback.rs".into()],
            diff_stat: "1 file changed".into(),
            diff: "diff --git a/src/fallback.rs b/src/fallback.rs\nnew file mode 100644\n--- /dev/null\n+++ b/src/fallback.rs\n+fn render() {}\n".into(),
            commit_style: CommitStyle::Standard,
            conventional_preset: default_preset(),
        });

        assert_eq!(options[0], "Improve fallback");
        assert_eq!(options[1], "Refine fallback behavior");
        assert_eq!(options[2], "Clean up fallback flow");
    }

    #[test]
    fn builds_conventional_heuristic_commit_options() {
        let options = build_heuristic_commit_options(&super::PromptInput {
            branch: "feature/release".into(),
            staged_files: vec!["src/tui.rs".into()],
            diff_stat: "1 file changed".into(),
            diff: "diff --git".into(),
            commit_style: CommitStyle::Conventional,
            conventional_preset: default_preset(),
        });

        assert!(options[0].starts_with("feat(tui): "));
        for option in options {
            validate_commit_message(&option, CommitStyle::Conventional).unwrap();
        }
    }

    #[test]
    fn builds_split_suggestions_for_mixed_concerns() {
        let suggestions = build_heuristic_commit_suggestions(&super::PromptInput {
            branch: "feature/billing".into(),
            staged_files: vec!["src/billing.rs".into(), "src/subscription.rs".into()],
            diff_stat: "2 files changed".into(),
            diff: "diff --git a/src/billing.rs b/src/billing.rs\n+fn billing_summary_card() {}\ndiff --git a/src/subscription.rs b/src/subscription.rs\n+if status == null {\n+    return;\n+}\n".into(),
            commit_style: CommitStyle::Conventional,
            conventional_preset: default_preset(),
        });

        assert_eq!(suggestions.split.len(), 2);
        assert!(suggestions.split[0].message.contains("billing"));
        assert_eq!(
            suggestions.split[0].files,
            vec!["src/billing.rs".to_string()]
        );
        assert!(suggestions.split[1].message.contains("subscription"));
        assert_eq!(
            suggestions.split[1].files,
            vec!["src/subscription.rs".to_string()]
        );
    }

    #[test]
    fn builds_standard_split_suggestions_for_docs_and_tests() {
        let suggestions = build_heuristic_commit_suggestions(&super::PromptInput {
            branch: "main".into(),
            staged_files: vec!["README.md".into(), "tests/ai_provider.rs".into()],
            diff_stat: "2 files changed".into(),
            diff: "diff --git a/README.md b/README.md\n+Add split commit guidance\ndiff --git a/tests/ai_provider.rs b/tests/ai_provider.rs\n+fn covers_split_suggestions() {}\n".into(),
            commit_style: CommitStyle::Standard,
            conventional_preset: default_preset(),
        });

        assert_eq!(
            suggestions.split,
            vec![
                SplitCommitPlan {
                    message: "Update documentation".to_string(),
                    files: vec!["README.md".to_string()],
                },
                SplitCommitPlan {
                    message: "Expand ai provider coverage".to_string(),
                    files: vec!["tests/ai_provider.rs".to_string()],
                },
            ]
        );
    }

    #[test]
    fn avoids_split_suggestions_for_single_concern() {
        let suggestions = build_heuristic_commit_suggestions(&super::PromptInput {
            branch: "feature/tui".into(),
            staged_files: vec!["src/tui.rs".into(), "src/tui/input.rs".into()],
            diff_stat: "2 files changed".into(),
            diff: "diff --git a/src/tui.rs b/src/tui.rs\n+fn render_commit_view() {}\ndiff --git a/src/tui/input.rs b/src/tui/input.rs\n+fn handle_commit_input() {}\n".into(),
            commit_style: CommitStyle::Conventional,
            conventional_preset: default_preset(),
        });

        assert!(suggestions.split.is_empty());
    }

    #[test]
    fn heuristic_conventional_types_follow_custom_preset() {
        let suggestions = build_heuristic_commit_suggestions(&super::PromptInput {
            branch: "fix/parser".into(),
            staged_files: vec!["src/parser.rs".into()],
            diff_stat: "1 file changed".into(),
            diff: "diff --git a/src/parser.rs b/src/parser.rs\n+// fix parser edge case".into(),
            commit_style: CommitStyle::Conventional,
            conventional_preset: ResolvedConventionalPreset {
                name: "team".into(),
                types: vec!["bugfix".into(), "maintenance".into()],
            },
        });

        assert!(suggestions.options[0].starts_with("bugfix(parser): "));
    }

    #[test]
    fn parses_ask_suggestion_json() {
        let raw = r#"{"recommended":[{"command":"git reset --soft HEAD~1","description":"Undo last commit, keep changes staged"}],"alternative":[{"command":"git revert HEAD","description":"Create a revert commit"}],"explanation":"reset --soft moves HEAD back one commit.","teaching_note":"--soft preserves the index."}"#;
        let suggestion = parse_ask_suggestion(raw).unwrap();

        assert_eq!(
            suggestion,
            AskSuggestion {
                recommended: vec![SuggestedCommand {
                    command: "git reset --soft HEAD~1".into(),
                    description: "Undo last commit, keep changes staged".into(),
                }],
                alternative: Some(vec![SuggestedCommand {
                    command: "git revert HEAD".into(),
                    description: "Create a revert commit".into(),
                }]),
                explanation: "reset --soft moves HEAD back one commit.".into(),
                teaching_note: "--soft preserves the index.".into(),
            }
        );
    }

    #[test]
    fn parses_ask_suggestion_with_null_alternative() {
        let raw = r#"{"recommended":[{"command":"git status","description":"Show repo status"}],"alternative":null,"explanation":"Shows staged and unstaged changes.","teaching_note":"Use regularly to orient yourself."}"#;
        let suggestion = parse_ask_suggestion(raw).unwrap();

        assert!(suggestion.alternative.is_none());
    }

    #[test]
    fn rejects_ask_suggestion_with_non_git_command() {
        let raw = r#"{"recommended":[{"command":"rm -rf .git","description":"Delete git repo"}],"alternative":null,"explanation":"...","teaching_note":"..."}"#;
        let error = parse_ask_suggestion(raw).unwrap_err();

        assert!(error.to_string().contains("does not start with 'git '"));
    }

    #[test]
    fn builds_ask_user_prompt_with_context() {
        let context = AskContext {
            branch: "main".into(),
            staged_count: 2,
            unstaged_count: 1,
            recent_log: "abc1234 feat: add login\ndef5678 fix: auth bug".into(),
        };
        let prompt = build_ask_user_prompt("undo last commit", &context);

        assert!(prompt.contains("undo last commit"));
        assert!(prompt.contains("Branch: main"));
        assert!(prompt.contains("Staged files: 2"));
        assert!(prompt.contains("abc1234 feat: add login"));
    }
}
