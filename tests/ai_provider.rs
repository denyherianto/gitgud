use std::time::Duration;

use gitgud::ai::{AiClient, AiConfig, PromptInput};
use gitgud::config::{CommitStyle, Provider};
use mockito::{Matcher, Server};

fn prompt() -> PromptInput {
    PromptInput {
        branch: "main".into(),
        staged_files: vec!["src/main.rs".into()],
        diff_stat: "1 file changed".into(),
        diff: "diff --git a/src/main.rs b/src/main.rs".into(),
        commit_style: CommitStyle::Standard,
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
    )
    .unwrap();
    let client = AiClient::new(config).unwrap();
    let options = client.generate_commit_options(&prompt()).await.unwrap();

    assert_eq!(options.len(), 3);
    assert_eq!(options[0], "Add TUI commit flow");
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
    )
    .unwrap()
    .with_timeout(Duration::from_millis(50));
    let client = AiClient::new(config).unwrap();
    let error = client.generate_commit_options(&prompt()).await.unwrap_err();

    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("timed out")
            || rendered.contains("deadline")
            || rendered.contains("operation timeout"),
        "unexpected timeout error: {rendered}"
    );
}
