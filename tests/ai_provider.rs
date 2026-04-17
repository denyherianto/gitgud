use std::time::Duration;

use gitgud::ai::{AiClient, AiConfig, DiffExplanation, PromptInput, fetch_model_options};
use gitgud::config::{CommitStyle, GenerationMode, Provider, ResolvedConventionalPreset};
use mockito::{Matcher, Server};

fn prompt() -> PromptInput {
    PromptInput {
        branch: "main".into(),
        staged_files: vec!["src/main.rs".into()],
        diff_stat: "1 file changed".into(),
        diff: "diff --git a/src/main.rs b/src/main.rs".into(),
        commit_style: CommitStyle::Standard,
        conventional_preset: ResolvedConventionalPreset::built_in_default(),
    }
}

#[tokio::test]
async fn generates_commit_message_from_mock_server() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("POST", "/chat/completions")
        .match_header("authorization", "Bearer token")
        .match_body(Matcher::PartialJson(serde_json::json!({
            "model": "model",
        })))
        .with_status(200)
        .with_body(r#"{"choices":[{"message":{"content":"{\"options\":[\"Add TUI commit flow\",\"Refine push handling\",\"Improve config setup\"]}"}}]}"#)
        .create_async()
        .await;

    let config = AiConfig::new(
        "token".into(),
        Provider::Gemini,
        &server.url(),
        "model",
        CommitStyle::Standard,
        GenerationMode::Auto,
        ResolvedConventionalPreset::built_in_default(),
    )
    .unwrap();
    let client = AiClient::new(config).unwrap();
    let options = client.generate_commit_options(&prompt()).await.unwrap();

    assert_eq!(options.len(), 3);
    assert_eq!(options[0], "Add TUI commit flow");
}

#[tokio::test]
async fn surfaces_split_commit_suggestions_from_mock_server() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("POST", "/chat/completions")
        .match_header("authorization", "Bearer token")
        .with_status(200)
        .with_body(r#"{"choices":[{"message":{"content":"{\"options\":[\"feat(billing): update billing and subscription flow\"],\"split\":[\"feat(billing): add billing summary card\",\"fix(subscription): handle null subscription status\"]}"}}]}"#)
        .create_async()
        .await;

    let config = AiConfig::new(
        "token".into(),
        Provider::Gemini,
        &server.url(),
        "model",
        CommitStyle::Conventional,
        GenerationMode::Auto,
        ResolvedConventionalPreset::built_in_default(),
    )
    .unwrap();
    let client = AiClient::new(config).unwrap();
    let suggestions = client
        .generate_commit_suggestions(&PromptInput {
            branch: "feature/billing".into(),
            staged_files: vec!["src/billing.rs".into(), "src/subscription.rs".into()],
            diff_stat: "2 files changed".into(),
            diff: "diff --git a/src/billing.rs b/src/billing.rs\n+fn billing_summary_card() {}\ndiff --git a/src/subscription.rs b/src/subscription.rs\n+if status == null {\n+    return;\n+}\n".into(),
            commit_style: CommitStyle::Conventional,
            conventional_preset: ResolvedConventionalPreset::built_in_default(),
        })
        .await
        .unwrap();

    assert_eq!(suggestions.options.len(), 1);
    assert_eq!(suggestions.split.len(), 2);
    assert_eq!(
        suggestions.split[0].message,
        "feat(billing): add billing summary card"
    );
    assert_eq!(
        suggestions.split[0].files,
        vec!["src/billing.rs".to_string()]
    );
}

#[tokio::test]
async fn explains_diff_from_mock_server() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("POST", "/chat/completions")
        .match_header("authorization", "Bearer token")
        .with_status(200)
        .with_body(r#"{"choices":[{"message":{"content":"{\"what_changed\":[\"Adds a new explain command\"],\"possible_intent\":[\"Help users understand staged diffs before committing\"],\"risk_areas\":[\"Explanations could overfit noisy diffs\"],\"test_suggestions\":[\"Cover the parser and command path with mocked responses\"]}"}}]}"#)
        .create_async()
        .await;

    let config = AiConfig::new(
        "token".into(),
        Provider::Gemini,
        &server.url(),
        "model",
        CommitStyle::Standard,
        GenerationMode::Auto,
        ResolvedConventionalPreset::built_in_default(),
    )
    .unwrap();
    let client = AiClient::new(config).unwrap();
    let explanation = client.generate_diff_explanation(&prompt()).await.unwrap();

    assert_eq!(
        explanation,
        DiffExplanation {
            what_changed: vec!["Adds a new explain command".to_string()],
            possible_intent: vec![
                "Help users understand staged diffs before committing".to_string()
            ],
            risk_areas: vec!["Explanations could overfit noisy diffs".to_string()],
            test_suggestions: vec![
                "Cover the parser and command path with mocked responses".to_string()
            ],
        }
    );
}

#[tokio::test]
async fn surfaces_auth_errors() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("POST", "/chat/completions")
        .with_status(401)
        .with_body("unauthorized")
        .create_async()
        .await;

    let config = AiConfig::new(
        "token".into(),
        Provider::Gemini,
        &server.url(),
        "model",
        CommitStyle::Standard,
        GenerationMode::Auto,
        ResolvedConventionalPreset::built_in_default(),
    )
    .unwrap();
    let client = AiClient::new(config).unwrap();
    let error = client.generate_commit_options(&prompt()).await.unwrap_err();

    assert!(error.to_string().contains("401"));
}

#[tokio::test]
async fn surfaces_rate_limits() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("POST", "/chat/completions")
        .with_status(429)
        .with_body("rate limited")
        .create_async()
        .await;

    let config = AiConfig::new(
        "token".into(),
        Provider::Gemini,
        &server.url(),
        "model",
        CommitStyle::Standard,
        GenerationMode::Auto,
        ResolvedConventionalPreset::built_in_default(),
    )
    .unwrap();
    let client = AiClient::new(config).unwrap();
    let error = client.generate_commit_options(&prompt()).await.unwrap_err();

    assert!(error.to_string().contains("429"));
}

#[tokio::test]
async fn rejects_malformed_json() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_body("{nope")
        .create_async()
        .await;

    let config = AiConfig::new(
        "token".into(),
        Provider::Gemini,
        &server.url(),
        "model",
        CommitStyle::Standard,
        GenerationMode::Auto,
        ResolvedConventionalPreset::built_in_default(),
    )
    .unwrap();
    let client = AiClient::new(config).unwrap();
    let error = client.generate_commit_options(&prompt()).await.unwrap_err();

    assert!(error.to_string().contains("parse AI response JSON"));
}

#[tokio::test]
async fn times_out_when_server_hangs() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_chunked_body(|writer| {
            std::thread::sleep(Duration::from_millis(200));
            writer.write_all(b"{\"choices\":[]}")
        })
        .create_async()
        .await;

    let config = AiConfig::new(
        "token".into(),
        Provider::Gemini,
        &server.url(),
        "model",
        CommitStyle::Standard,
        GenerationMode::Auto,
        ResolvedConventionalPreset::built_in_default(),
    )
    .unwrap()
    .with_timeout(Duration::from_millis(50));
    let client = AiClient::new(config).unwrap();
    let suggestions = client.generate_commit_suggestions(&prompt()).await.unwrap();

    assert_eq!(suggestions.options.len(), 3);
    assert_eq!(suggestions.options[0], "Improve main");
    assert_eq!(
        suggestions.note.as_deref(),
        Some("AI timed out. Showing heuristic commit options.")
    );
}

#[tokio::test]
async fn heuristic_only_skips_ai_requests() {
    let config = AiConfig::new(
        "token".into(),
        Provider::Gemini,
        "https://example.com",
        "model",
        CommitStyle::Standard,
        GenerationMode::HeuristicOnly,
        ResolvedConventionalPreset::built_in_default(),
    )
    .unwrap();
    let client = AiClient::new(config).unwrap();
    let suggestions = client.generate_commit_suggestions(&prompt()).await.unwrap();

    assert_eq!(suggestions.options[0], "Improve main");
    assert_eq!(
        suggestions.note.as_deref(),
        Some("Using heuristic commit options only.")
    );
}

#[tokio::test]
async fn ai_only_surfaces_timeout_without_fallback() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_chunked_body(|writer| {
            std::thread::sleep(Duration::from_millis(200));
            writer.write_all(b"{\"choices\":[]}")
        })
        .create_async()
        .await;

    let config = AiConfig::new(
        "token".into(),
        Provider::Gemini,
        &server.url(),
        "model",
        CommitStyle::Standard,
        GenerationMode::AiOnly,
        ResolvedConventionalPreset::built_in_default(),
    )
    .unwrap()
    .with_timeout(Duration::from_millis(50));
    let client = AiClient::new(config).unwrap();
    let error = client
        .generate_commit_suggestions(&prompt())
        .await
        .unwrap_err();

    assert!(error.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_timeout)
    }));
}

#[tokio::test]
async fn lists_models_from_provider() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/models")
        .match_header("authorization", "Bearer token")
        .with_status(200)
        .with_body(r#"{"data":[{"id":"gpt-4.1-mini"},{"id":"gpt-4.1"},{"id":"gpt-4.1-mini"}]}"#)
        .create_async()
        .await;

    let models = fetch_model_options(&server.url(), "token").await.unwrap();

    assert_eq!(models, vec!["gpt-4.1-mini", "gpt-4.1"]);
}

#[tokio::test]
async fn surfaces_model_listing_errors() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/models")
        .with_status(401)
        .with_body("unauthorized")
        .create_async()
        .await;

    let error = fetch_model_options(&server.url(), "token")
        .await
        .unwrap_err();

    assert!(error.to_string().contains("listing models"));
    assert!(error.to_string().contains("401"));
}
