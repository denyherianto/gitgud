use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};

use crate::config::{CommitStyle, Provider, resolve_ai_settings};

pub const DEFAULT_PROVIDER: Provider = Provider::Gemini;
pub const DEFAULT_BASE_API_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";
pub const DEFAULT_BASE_MODEL: &str = "gemini-2.5-flash";
pub const DEFAULT_COMMIT_STYLE: CommitStyle = CommitStyle::Standard;
const DEFAULT_TIMEOUT_SECS: u64 = 20;
const DEFAULT_MAX_DIFF_CHARS: usize = 12_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiConfig {
    pub api_token: String,
    pub provider: Provider,
    pub base_api_url: String,
    pub base_model: String,
    pub commit_style: CommitStyle,
    pub timeout: Duration,
}

impl AiConfig {
    pub fn load() -> Result<Self> {
        let resolved = resolve_ai_settings()?;
        Self::new(
            resolved.api_token.value,
            resolved.provider.value,
            &resolved.base_api_url.value,
            &resolved.base_model.value,
            resolved.commit_style.value,
        )
    }

    pub fn new(
        api_token: String,
        provider: Provider,
        base_api_url: &str,
        base_model: &str,
        commit_style: CommitStyle,
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

    pub async fn generate_commit_message(&self, input: &PromptInput) -> Result<String> {
        let mut options = self.generate_commit_options(input).await?;
        options
            .drain(..)
            .next()
            .ok_or_else(|| anyhow!("AI provider returned no commit message options"))
    }

    pub async fn generate_commit_options(&self, input: &PromptInput) -> Result<Vec<String>> {
        match self.generate_commit_options_from_provider(input).await {
            Ok(options) => Ok(options),
            Err(error) if is_timeout_error(&error) => Ok(build_heuristic_commit_options(input)),
            Err(error) => Err(error),
        }
    }

    async fn generate_commit_options_from_provider(
        &self,
        input: &PromptInput,
    ) -> Result<Vec<String>> {
        let endpoint = format!("{}/chat/completions", self.config.base_api_url);
        let request = ChatCompletionRequest {
            model: self.config.base_model.clone(),
            temperature: 0.2,
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: build_system_prompt(input.commit_style),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: build_commit_prompt(input),
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
        let content = parsed
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .ok_or_else(|| anyhow!("AI provider returned no choices"))?;

        parse_commit_options(&content, input.commit_style)
    }
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
    let file_list = if input.staged_files.is_empty() {
        "(none)".to_string()
    } else {
        input.staged_files.join(", ")
    };

    let style = match input.commit_style {
        CommitStyle::Standard => "standard",
        CommitStyle::Conventional => "conventional",
    };

    format!(
        "Commit style: {style}\nBranch: {}\nStaged files: {}\nDiff summary:\n{}\n\nStaged patch:\n{}",
        input.branch,
        file_list,
        if input.diff_stat.trim().is_empty() {
            "(no diff summary)"
        } else {
            input.diff_stat.trim()
        },
        truncate_diff(&input.diff, DEFAULT_MAX_DIFF_CHARS)
    )
}

pub fn truncate_diff(diff: &str, max_chars: usize) -> String {
    if diff.chars().count() <= max_chars {
        return diff.to_string();
    }

    let truncated: String = diff.chars().take(max_chars).collect();
    format!("{truncated}\n\n[diff truncated]")
}

pub fn validate_commit_message(raw: &str, commit_style: CommitStyle) -> Result<String> {
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
        validate_conventional_subject(subject)?;
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

pub fn build_heuristic_commit_options(input: &PromptInput) -> Vec<String> {
    let subject = describe_change_subject(&input.staged_files);
    let standard = [
        format!("Update {subject}"),
        format!("Refine {subject} handling"),
        format!("Adjust {subject} flow"),
    ];

    let conventional_type = infer_conventional_type(input);
    let conventional_scope = infer_conventional_scope(&input.staged_files);
    let conventional = [
        format!("{conventional_type}({conventional_scope}): update {subject}"),
        format!("{conventional_type}({conventional_scope}): refine {subject} handling"),
        format!("{conventional_type}({conventional_scope}): adjust {subject} flow"),
    ];

    let candidates = match input.commit_style {
        CommitStyle::Standard => standard,
        CommitStyle::Conventional => conventional,
    };

    candidates.into_iter().map(limit_subject_to_72).collect()
}

fn build_system_prompt(commit_style: CommitStyle) -> String {
    let style_rules = match commit_style {
        CommitStyle::Standard => {
            "Use standard commit messages with an imperative subject under 72 characters."
        }
        CommitStyle::Conventional => {
            "Use Conventional Commits. Every subject must match type(scope optional)!: description."
        }
    };

    format!(
        "You write concise Git commit messages. Return valid JSON only with this shape: {{\"options\":[\"message 1\",\"message 2\",\"message 3\"]}}. Provide 1 to 3 distinct options. Each option may include a blank line and body, but no markdown fences, labels, numbering, or commentary. Describe only the staged changes. {style_rules}"
    )
}

fn parse_commit_options(raw: &str, commit_style: CommitStyle) -> Result<Vec<String>> {
    let parsed = parse_options_payload(raw)?;
    if !(1..=3).contains(&parsed.options.len()) {
        bail!("AI provider must return between 1 and 3 commit message options");
    }

    let mut options = Vec::with_capacity(parsed.options.len());
    for option in parsed.options {
        options.push(validate_commit_message(&option, commit_style)?);
    }

    Ok(options)
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

fn validate_conventional_subject(subject: &str) -> Result<()> {
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

fn is_timeout_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_timeout)
    })
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

fn infer_conventional_type(input: &PromptInput) -> &'static str {
    let branch = input.branch.to_ascii_lowercase();

    if input
        .staged_files
        .iter()
        .any(|path| path.starts_with(".github/"))
    {
        "ci"
    } else if input
        .staged_files
        .iter()
        .all(|path| path == "README.md" || path.starts_with("docs/"))
    {
        "docs"
    } else if input
        .staged_files
        .iter()
        .all(|path| path.starts_with("tests/"))
    {
        "test"
    } else if branch.starts_with("fix/")
        || branch.starts_with("bugfix/")
        || branch.starts_with("hotfix/")
    {
        "fix"
    } else if branch.starts_with("feat/") || branch.starts_with("feature/") {
        "feat"
    } else {
        "chore"
    }
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
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        AiConfig, DEFAULT_BASE_API_URL, build_commit_prompt, build_heuristic_commit_options,
        normalize_base_api_url, parse_commit_options, truncate_diff, validate_commit_message,
    };
    use crate::config::{CommitStyle, Provider};

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
        });

        assert!(prompt.contains("Branch: main"));
        assert!(prompt.contains("Commit style: standard"));
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("1 file changed"));
    }

    #[test]
    fn supports_test_timeout_override() {
        let config = AiConfig::new(
            "token".into(),
            Provider::Gemini,
            DEFAULT_BASE_API_URL,
            "model",
            CommitStyle::Standard,
        )
        .unwrap()
        .with_timeout(Duration::from_secs(1));
        assert_eq!(config.timeout, Duration::from_secs(1));
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
    fn validates_conventional_commit_subjects() {
        let message = validate_commit_message(
            "feat(cli): add multiple commit message options",
            CommitStyle::Conventional,
        )
        .unwrap();

        assert_eq!(message, "feat(cli): add multiple commit message options");
    }

    #[test]
    fn builds_standard_heuristic_commit_options() {
        let options = build_heuristic_commit_options(&super::PromptInput {
            branch: "main".into(),
            staged_files: vec![".github/workflows/release.yml".into()],
            diff_stat: "1 file changed".into(),
            diff: "diff --git".into(),
            commit_style: CommitStyle::Standard,
        });

        assert_eq!(options[0], "Update release workflow");
        assert_eq!(options.len(), 3);
    }

    #[test]
    fn builds_conventional_heuristic_commit_options() {
        let options = build_heuristic_commit_options(&super::PromptInput {
            branch: "feature/release".into(),
            staged_files: vec!["src/tui.rs".into()],
            diff_stat: "1 file changed".into(),
            diff: "diff --git".into(),
            commit_style: CommitStyle::Conventional,
        });

        assert!(options[0].starts_with("feat(tui): "));
        for option in options {
            validate_commit_message(&option, CommitStyle::Conventional).unwrap();
        }
    }
}
