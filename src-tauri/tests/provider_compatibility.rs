use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};
use transitpls_lib::config::LlmConfig;
use transitpls_lib::llm::{RecordingClient, RigClient, TranslationClient};
use transitpls_lib::usage::UsageRecorder;

fn server(status: u16, body: String) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let handle = std::thread::spawn(move || {
        let started = Instant::now();
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        started.elapsed() < Duration::from_secs(10),
                        "no request received"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("{error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        loop {
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&buffer[..count]);
            if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]);
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        String::from_utf8(request).unwrap()
    });
    (url, handle)
}

#[tokio::test]
async fn chat_compatibility_matrix_records_usage_and_request_metadata() {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/chat-compatible.json")).unwrap();
    for fixture in fixtures.as_array().unwrap() {
        let (url, server) = server(200, fixture["response"].to_string());
        let root = std::env::temp_dir().join(format!(
            "transitpls-provider-{}-{}",
            std::process::id(),
            fixture["name"].as_str().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = LlmConfig {
            provider: fixture["provider"].as_str().unwrap().into(),
            base_url: Some(format!("{url}{}", fixture["base_path"].as_str().unwrap())),
            model: "fixture-model".into(),
            api_key_env: String::new(),
            ..LlmConfig::default()
        };
        let recorder = UsageRecorder::new(&root, &config.model);
        let client = RigClient::from_config_with_api_key(&config, fixture["key"].as_str().unwrap())
            .unwrap()
            .with_recorder(recorder.clone());
        let client = RecordingClient::new(Box::new(client), recorder);
        let result = client
            .complete_attempt("private system prompt", "private user prompt", 2)
            .await;
        let request = server.join().unwrap();
        let output = result.unwrap_or_else(|error| panic!("{}: {error}", fixture["name"]));
        assert_eq!(output.text, "translated text");
        assert!(request.starts_with(&format!(
            "POST {} HTTP/1.1",
            fixture["path"].as_str().unwrap()
        )));
        let (headers, body) = request.split_once("\r\n\r\n").unwrap();
        if fixture["key"] == "" {
            assert!(!headers.to_lowercase().contains("authorization:"));
        } else {
            assert!(headers
                .to_lowercase()
                .contains("authorization: bearer fixture-key"));
        }
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["model"], "fixture-model");
        assert_eq!(body["messages"][0]["role"], "system");
        assert!(body["messages"].to_string().contains("private user prompt"));
        let logs = std::fs::read_to_string(root.join("logs.txt")).unwrap();
        assert!(logs.contains("request_completed") && logs.contains("\"retry_count\":2"));
        assert!(
            !logs.contains("private")
                && !logs.contains("fixture-key")
                && !logs.contains("translated text")
        );
        let missing = fixture["response"].get("usage").is_none();
        assert_eq!(logs.contains("usage_missing"), missing);
        assert_eq!(output.usage.total_tokens, if missing { 0 } else { 14 });
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn errors_are_contextual_redacted_and_recorded() {
    for (index, (status, body, expected)) in [
        (401, "<html>secret-key private prompt</html>", "authentication failed"),
        (404, "{\"error\":\"secret-key\"}", "model or endpoint not found"),
        (200, "not JSON secret-key", "response_parse_failed"),
        (200, r#"{"id":"test","model":"fixture-model","choices":[{"index":0,"message":{"role":"assistant","content":" "},"finish_reason":"stop"}]}"#, "response_parse_failed"),
        (200, r#"{"id":"test","model":"fixture-model","choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call","type":"function","function":{"name":"tool","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#, "response_parse_failed"),
    ].into_iter().enumerate() {
        let (url, server) = server(status, body.into());
        let root = std::env::temp_dir().join(format!("transitpls-error-{}-{index}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let config = LlmConfig { base_url: Some(url), model: "fixture-model".into(), ..LlmConfig::default() };
        let recorder = UsageRecorder::new(&root, &config.model);
        let client = RigClient::from_config_with_api_key(&config, "secret-key").unwrap().with_recorder(recorder.clone());
        let client = RecordingClient::new(Box::new(client), recorder);
        let error = client.complete("private prompt", "private prompt").await.unwrap_err();
        server.join().unwrap();
        assert!(error.contains(expected), "{error}");
        assert!(error.contains("provider=openai-chat") && error.contains("model=fixture-model") && error.contains("stage=translation"));
        let logs = std::fs::read_to_string(root.join("logs.txt")).unwrap();
        assert!(logs.contains("llm_failed"));
        for value in [&error, &logs] { assert!(!value.contains("secret-key") && !value.contains("private prompt")); }
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn native_protocols_keep_their_paths_authentication_and_usage() {
    for (provider, suffix, header, body) in [
        (
            "openai-responses",
            "/v1/responses",
            "authorization: bearer fixture-key",
            serde_json::json!({
                "id":"resp-test", "object":"response", "created_at":1, "status":"completed", "model":"fixture-model",
                "output":[{"id":"msg-test","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"OK","annotations":[]}]}],
                "usage":{"input_tokens":10,"output_tokens":4,"total_tokens":14}
            }),
        ),
        (
            "anthropic",
            "/v1/messages",
            "x-api-key: fixture-key",
            serde_json::json!({
                "id":"msg-test", "type":"message", "role":"assistant", "model":"fixture-model",
                "content":[{"type":"text","text":"OK"}], "stop_reason":"end_turn", "stop_sequence":null,
                "usage":{"input_tokens":10,"output_tokens":4}
            }),
        ),
    ] {
        let (url, server) = server(200, body.to_string());
        let config = LlmConfig {
            provider: provider.into(),
            base_url: Some(format!("{url}{suffix}")),
            model: "fixture-model".into(),
            ..LlmConfig::default()
        };
        let client = RigClient::from_config_with_api_key(&config, "fixture-key").unwrap();
        let result = client.complete("system", "user").await;
        let request = server.join().unwrap();
        assert!(request.starts_with(&format!("POST {suffix} HTTP/1.1")));
        assert!(request.to_lowercase().contains(header));
        let result = result.unwrap_or_else(|error| panic!("{provider}: {error}"));
        assert_eq!(result.text, "OK");
        assert_eq!(result.usage.input_tokens, 10);
        assert_eq!(result.usage.output_tokens, 4);
    }
}

#[test]
fn configuration_normalization_and_presets() {
    for (provider, input, expected) in [
        (
            " OpenAI-Compatible ",
            "https://api.deepseek.com/v1/chat/completions/",
            "https://api.deepseek.com/v1",
        ),
        (
            "openai-chat",
            "https://openrouter.ai/api/v1/",
            "https://openrouter.ai/api/v1",
        ),
        (
            "openai-responses",
            "https://api.openai.com/v1/responses",
            "https://api.openai.com/v1",
        ),
        (
            "anthropic",
            "https://api.anthropic.com/v1/messages",
            "https://api.anthropic.com",
        ),
    ] {
        let config = LlmConfig {
            provider: provider.into(),
            base_url: Some(input.into()),
            ..LlmConfig::default()
        }
        .normalized()
        .unwrap();
        assert_eq!(config.provider, provider.trim().to_lowercase());
        assert_eq!(config.base_url.as_deref(), Some(expected));
        assert_eq!(config.normalized().unwrap().base_url, config.base_url);
    }
    for input in [
        "ftp://example.com",
        "not-a-url-secret",
        "https://user:secret@example.com/v1",
        "https://example.com/v1?key=secret",
        "https://example.com/v1/responses",
        "https://api.anthropic.com",
    ] {
        let error = LlmConfig {
            base_url: Some(input.into()),
            ..LlmConfig::default()
        }
        .normalized()
        .unwrap_err();
        assert!(error.contains("configuration_failed") && !error.contains("secret"));
    }
    assert!(LlmConfig {
        provider: "unknown".into(),
        ..LlmConfig::default()
    }
    .normalized()
    .unwrap_err()
    .contains("openai-compatible"));
    for host in ["localhost", "127.0.0.1", "[::1]"] {
        let config = LlmConfig {
            provider: "openai-compatible".into(),
            base_url: Some(format!("http://{host}:11434/v1")),
            api_key_env: String::new(),
            ..LlmConfig::default()
        };
        assert_eq!(config.api_key().unwrap(), "");
    }
    assert!(RigClient::from_config_with_api_key(&LlmConfig::default(), "").is_err());
    let presets: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../src/provider-presets.json")).unwrap();
    assert_eq!(presets.len(), 8);
    for preset in presets {
        if preset["base_url"] == "" {
            continue;
        }
        let config: LlmConfig = serde_json::from_value(preset).unwrap();
        config.normalized().unwrap();
    }
}
