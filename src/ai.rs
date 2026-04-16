use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};

use crate::config::resolve_ai_settings;

pub const DEFAULT_BASE_API_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";
pub const DEFAULT_BASE_MODEL: &str = "gemini-2.5-flash";
const SYSTEM_PROMPT: &str = "You write concise Git commit messages. Return only the commit message. The first line must be an imperative subject under 72 characters. Optionally include a blank line and a body. Describe only the staged changes. Never use markdown fences, labels, or commentary.";
const DEFAULT_TIMEOUT_SECS: u64 = 20;
const DEFAULT_MAX_DIFF_CHARS: usize = 12_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiConfig {
    pub api_token: String,
    pub base_api_url: String,
    pub base_model: String,
    pub timeout: Duration,
}

impl AiConfig {
    pub fn load() -> Result<Self> {
        let resolved = resolve_ai_settings()?;
        Self::new(
            resolved.api_token.value,
            &resolved.base_api_url.value,
            &resolved.base_model.value,
        )
    }

    pub fn new(api_token: String, base_api_url: &str, base_model: &str) -> Result<Self> {
        if api_token.trim().is_empty() {
            bail!("API_TOKEN cannot be empty");
        }

        if base_model.trim().is_empty() {
            bail!("BASE_MODEL cannot be empty");
        }

        Ok(Self {
            api_token,
            base_api_url: normalize_base_api_url(base_api_url)?,
            base_model: base_model.trim().to_string(),
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
        let endpoint = format!("{}/chat/completions", self.config.base_api_url);
        let request = ChatCompletionRequest {
            model: self.config.base_model.clone(),
            temperature: 0.2,
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: SYSTEM_PROMPT.to_string(),
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

        validate_commit_message(&content)
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

    format!(
        "Branch: {}\nStaged files: {}\nDiff summary:\n{}\n\nStaged patch:\n{}",
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

pub fn validate_commit_message(raw: &str) -> Result<String> {
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        AiConfig, DEFAULT_BASE_API_URL, build_commit_prompt, normalize_base_api_url, truncate_diff,
        validate_commit_message,
    };

    #[test]
    fn normalizes_base_url() {
        let actual = normalize_base_api_url("https://example.com/v1/").unwrap();
        assert_eq!(actual, "https://example.com/v1");
    }

    #[test]
    fn rejects_empty_model() {
        let error = AiConfig::new("token".into(), DEFAULT_BASE_API_URL, "   ").unwrap_err();
        assert!(error.to_string().contains("BASE_MODEL"));
    }

    #[test]
    fn trims_body_spacing() {
        let actual = validate_commit_message("Add TUI flow\n\nRefine navigation\n").unwrap();
        assert_eq!(actual, "Add TUI flow\n\nRefine navigation");
    }

    #[test]
    fn rejects_long_subject() {
        let subject = "a".repeat(73);
        let error = validate_commit_message(&subject).unwrap_err();
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
        });

        assert!(prompt.contains("Branch: main"));
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("1 file changed"));
    }

    #[test]
    fn supports_test_timeout_override() {
        let config = AiConfig::new("token".into(), DEFAULT_BASE_API_URL, "model")
            .unwrap()
            .with_timeout(Duration::from_secs(1));
        assert_eq!(config.timeout, Duration::from_secs(1));
    }
}
