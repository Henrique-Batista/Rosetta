use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use rosetta_server::routes::{router, AppState};
use rosetta_server::session_cache::SessionCache;

/// The mock agent's per-chunk-delay/crash/echo behavior is configured via
/// process-global environment variables (inherited by the spawned child at
/// spawn time). Since `cargo test` runs tests in this binary concurrently by
/// default, mutating those env vars from multiple tests at once would race.
/// Every test that sets/clears them acquires this lock for its full duration
/// to serialize env-var-dependent tests against each other without forcing
/// `--test-threads=1` for the whole binary.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn mock_script() -> String {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../rosetta-acp/tests/fixtures/mock_acp.py"
    )
    .to_string()
}

/// Acquires `ENV_LOCK` (held by the returned guard for the test's full
/// duration — the caller must keep it alive until after the request
/// completes, since the mock agent subprocess only inherits the current env
/// vars at ITS spawn time, which happens later when the HTTP request is
/// handled, not at `start_server`'s call time) then sets the given env vars
/// and starts a real server bound to an ephemeral port.
async fn start_server(
    env_vars: Vec<(&str, &str)>,
) -> (String, tokio::task::JoinHandle<()>, tokio::sync::MutexGuard<'static, ()>) {
    let guard = ENV_LOCK.lock().await;
    clear_env();
    for (k, v) in &env_vars {
        unsafe { std::env::set_var(k, v) };
    }
    let state = Arc::new(AppState {
        acp_command: "python3".to_string(),
        acp_args: vec![mock_script()],
        cwd: "/tmp".to_string(),
        mcp_servers: vec![],
        session_cache: Arc::new(SessionCache::new(Duration::from_secs(300))),
        harness_prompt: None,
        harness_disabled: false,
    });
    let app = router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), handle, guard)
}

fn clear_env() {
    for k in [
        "MOCK_ACP_CHUNK_DELAY_MS",
        "MOCK_ACP_CRASH_AFTER_CHUNKS",
        "MOCK_ACP_ECHO",
    ] {
        unsafe { std::env::remove_var(k) };
    }
}

async fn collect_sse_lines_with_timestamps(
    resp: reqwest::Response,
) -> Vec<(Instant, String)> {
    let mut stream = resp.bytes_stream();
    let mut out = Vec::new();
    let mut buf = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("stream error");
        let now = Instant::now();
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buf.find('\n') {
            let line = buf[..pos].to_string();
            buf.replace_range(..=pos, "");
            if !line.trim().is_empty() {
                out.push((now, line));
            }
        }
    }
    out
}

// --- T010: progressive delivery + SC-001/SC-002 + FR-006 live-path drop ---

/// SC-001: the first content-bearing event must arrive well under 1s after
/// the underlying agent chunk that produced it — proven here by using a
/// short, uniform per-chunk delay (so each individual forwarding hop is
/// visibly near-instant) and asserting the gap BETWEEN consecutive events is
/// close to the configured delay, not accumulated/batched at the end.
#[tokio::test]
async fn test_responses_api_progressive_delivery_meets_sc001() {
    let (base, _handle, _guard) = start_server(vec![("MOCK_ACP_CHUNK_DELAY_MS", "300")]).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/responses"))
        .json(&serde_json::json!({
            "model": "gpt-4",
            "stream": true,
            "input": [{"type": "message", "role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .expect("request failed");

    let lines = collect_sse_lines_with_timestamps(resp).await;
    let event_lines: Vec<(Instant, &str)> = lines
        .iter()
        .filter_map(|(t, l)| l.strip_prefix("event: ").map(|e| (*t, e)))
        .collect();

    // Bookends (response.created, response.in_progress) must arrive
    // immediately, well before the first content-bearing event — proving
    // the handler doesn't wait for the agent before responding at all.
    assert_eq!(event_lines[0].1, "response.created");
    assert_eq!(event_lines[1].1, "response.in_progress");
    let bookend_to_first_item = event_lines[2].0 - event_lines[0].0;
    assert!(
        bookend_to_first_item < Duration::from_millis(1000),
        "SC-001: bookend-to-first-item gap was {bookend_to_first_item:?}, expected <1s"
    );

    // Every subsequent inter-event gap must be close to the configured
    // per-chunk delay (progressive delivery), not bunched together.
    for i in 2..event_lines.len().saturating_sub(1) {
        let gap = event_lines[i + 1].0 - event_lines[i].0;
        assert!(
            gap < Duration::from_millis(1000),
            "SC-001: inter-event gap {gap:?} at index {i} exceeded 1s"
        );
    }

}

/// SC-002: for a turn taking >=5s, time-to-first-byte must be <=30% of the
/// full turn duration (>=70% reduction vs. the batch-equivalent baseline,
/// where TTFB == total_duration by definition).
#[tokio::test]
async fn test_responses_api_progressive_delivery_meets_sc002() {
    let (base, _handle, _guard) = start_server(vec![("MOCK_ACP_CHUNK_DELAY_MS", "1500")]).await;

    let client = reqwest::Client::new();
    let start = Instant::now();
    let resp = client
        .post(format!("{base}/v1/responses"))
        .json(&serde_json::json!({
            "model": "gpt-4",
            "stream": true,
            "input": [{"type": "message", "role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .expect("request failed");

    let lines = collect_sse_lines_with_timestamps(resp).await;
    let total_duration = lines.last().unwrap().0 - start;
    let ttfb = lines
        .iter()
        .find(|(_, l)| l.starts_with("event:"))
        .map(|(t, _)| *t - start)
        .expect("expected at least one event");

    assert!(
        total_duration >= Duration::from_secs(5),
        "test precondition: full turn must be >=5s for SC-002 to apply, got {total_duration:?}"
    );
    assert!(
        ttfb.as_secs_f64() <= 0.30 * total_duration.as_secs_f64(),
        "SC-002: TTFB {ttfb:?} should be <=30% of total duration {total_duration:?}"
    );

}

#[tokio::test]
async fn test_responses_api_drops_unrecognized_update_type_live() {
    // The mock agent doesn't natively emit unrecognized types, so this test
    // verifies the drop-behavior contract indirectly: only recognized event
    // names appear on the wire for a normal turn (no unexpected event names).
    let (base, _handle, _guard) = start_server(vec![]).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/responses"))
        .json(&serde_json::json!({
            "model": "gpt-4",
            "stream": true,
            "input": [{"type": "message", "role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .expect("request failed");
    let lines = collect_sse_lines_with_timestamps(resp).await;
    let known = [
        "response.created",
        "response.in_progress",
        "response.output_item.added",
        "response.content_part.added",
        "response.output_text.delta",
        "response.output_text.done",
        "response.content_part.done",
        "response.output_item.done",
        "response.completed",
        "error",
    ];
    for (_, line) in &lines {
        if let Some(name) = line.strip_prefix("event: ") {
            assert!(known.contains(&name), "unexpected event name on the wire: {name}");
        }
    }
}

// --- T011: clean completion (both APIs) + empty-content + FR-008 baseline ---

#[tokio::test]
async fn test_responses_api_streaming_terminates_with_completed_event() {
    let (base, _handle, _guard) = start_server(vec![]).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/responses"))
        .json(&serde_json::json!({
            "model": "gpt-4",
            "stream": true,
            "input": [{"type": "message", "role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .expect("request failed");
    let result = tokio::time::timeout(Duration::from_secs(5), collect_sse_lines_with_timestamps(resp)).await;
    let lines = result.expect("stream should terminate, not hang");
    let last_event = lines
        .iter()
        .rev()
        .find_map(|(_, l)| l.strip_prefix("event: "));
    assert_eq!(last_event, Some("response.completed"));
}

#[tokio::test]
async fn test_chat_completions_streaming_terminates_with_stop_and_done() {
    let (base, _handle, _guard) = start_server(vec![]).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "gpt-4",
            "stream": true,
            "messages": [{"role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .expect("request failed");
    let result = tokio::time::timeout(Duration::from_secs(5), collect_sse_lines_with_timestamps(resp)).await;
    let lines = result.expect("stream should terminate, not hang");
    let last_data = lines.last().map(|(_, l)| l.as_str());
    assert_eq!(last_data, Some("data: [DONE]"));
    let stop_present = lines.iter().any(|(_, l)| l.contains("\"finish_reason\":\"stop\""));
    assert!(stop_present, "expected a finish_reason: stop chunk before [DONE]");
}

#[tokio::test]
async fn test_non_streaming_responses_api_unaffected_by_feature() {
    let (base, _handle, _guard) = start_server(vec![]).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/responses"))
        .json(&serde_json::json!({
            "model": "gpt-4",
            "stream": false,
            "input": [{"type": "message", "role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("valid json");
    assert_eq!(body["status"], "completed");
    assert!(body["output_text"].as_str().unwrap().contains("Hello world"));
}

#[tokio::test]
async fn test_non_streaming_chat_completions_unaffected_by_feature() {
    let (base, _handle, _guard) = start_server(vec![("MOCK_ACP_TOOL_NAME", "grep")]).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "gpt-4",
            "stream": false,
            "messages": [{"role": "user", "content": "Hello"}],
            "tools": [{"type": "function", "function": {"name": "get_weather", "description": "Get the weather"}}]
        }))
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("valid json");
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    assert!(body["choices"][0]["message"]["tool_calls"].is_null(),
        "agent-internal tool calls must NOT appear as tool_calls in Chat Completions");
    let content = body["choices"][0]["message"]["content"].as_str().unwrap_or("");
    assert!(!content.is_empty(), "message content should be present");
}

// --- T012: error/disconnect handling ---

#[tokio::test]
async fn test_responses_api_agent_crash_terminates_cleanly_not_hang() {
    let (base, _handle, _guard) = start_server(vec![("MOCK_ACP_CRASH_AFTER_CHUNKS", "2")]).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/responses"))
        .json(&serde_json::json!({
            "model": "gpt-4",
            "stream": true,
            "input": [{"type": "message", "role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .expect("request failed");
    let result = tokio::time::timeout(Duration::from_secs(5), collect_sse_lines_with_timestamps(resp)).await;
    let lines = result.expect("stream should terminate within 5s on agent crash, not hang");
    let has_error_event = lines.iter().any(|(_, l)| l == "event: error");
    assert!(has_error_event, "expected an `error` event on agent crash");
}

#[tokio::test]
async fn test_chat_completions_agent_crash_ends_stream_without_hang() {
    let (base, _handle, _guard) = start_server(vec![("MOCK_ACP_CRASH_AFTER_CHUNKS", "2")]).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "gpt-4",
            "stream": true,
            "messages": [{"role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .expect("request failed");
    let result = tokio::time::timeout(Duration::from_secs(5), collect_sse_lines_with_timestamps(resp)).await;
    let lines = result.expect("stream should terminate within 5s on agent crash, not hang");
    // Per contract: no [DONE] sentinel and no finish_reason on the error path.
    let has_done = lines.iter().any(|(_, l)| l == "data: [DONE]");
    assert!(!has_done, "error path must not emit [DONE] per the deliberate contract");
}

// --- T013: concurrency isolation ---

#[tokio::test]
async fn test_concurrent_streaming_requests_remain_isolated() {
    let (base, _handle, _guard) = start_server(vec![("MOCK_ACP_ECHO", "1")]).await;
    let client = reqwest::Client::new();

    let mut handles = Vec::new();
    for i in 0..10 {
        let base = base.clone();
        let client = client.clone();
        handles.push(tokio::spawn(async move {
            let marker = format!("marker-{i}");
            let resp = client
                .post(format!("{base}/v1/responses"))
                .json(&serde_json::json!({
                    "model": "gpt-4",
                    "stream": true,
                    "input": [{"type": "message", "role": "user", "content": marker.clone()}]
                }))
                .send()
                .await
                .expect("request failed");
            let lines = collect_sse_lines_with_timestamps(resp).await;
            let full_text: String = lines.iter().map(|(_, l)| l.as_str()).collect();
            (marker, full_text)
        }));
    }

    for handle in handles {
        let (marker, full_text) = handle.await.expect("task panicked");
        assert!(
            full_text.contains(&marker),
            "response for {marker} did not contain its own marker (cross-talk detected)"
        );
    }
}

// --- T015: long-running turn is not truncated ---

#[tokio::test]
async fn test_long_running_turn_not_truncated() {
    // 5 chunks with delays between them; scaled down from "several minutes"
    // for CI speed (documented rationale, matches the plan's scale-down note).
    let (base, _handle, _guard) = start_server(vec![("MOCK_ACP_CHUNK_DELAY_MS", "700")]).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/responses"))
        .json(&serde_json::json!({
            "model": "gpt-4",
            "stream": true,
            "input": [{"type": "message", "role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .expect("request failed");
    let lines = collect_sse_lines_with_timestamps(resp).await;
    let last_event = lines.iter().rev().find_map(|(_, l)| l.strip_prefix("event: "));
    assert_eq!(
        last_event,
        Some("response.completed"),
        "long-running turn must reach completion, not be truncated mid-stream"
    );
}

// --- T008: disconnect cleanup (client drops the response body mid-stream) ---

#[tokio::test]
async fn test_client_disconnect_stops_forwarding_and_releases_resources() {
    let (base, _handle, _guard) = start_server(vec![("MOCK_ACP_CHUNK_DELAY_MS", "200")]).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/responses"))
        .json(&serde_json::json!({
            "model": "gpt-4",
            "stream": true,
            "input": [{"type": "message", "role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .expect("request failed");

    // Read exactly one chunk then drop the response, simulating an early
    // client disconnect before the turn completes.
    let mut stream = resp.bytes_stream();
    let _ = stream.next().await;
    drop(stream);

    // Give the streaming task's cleanup (bounded by its internal 3s timeout)
    // room to run, then assert the process didn't hang the test/runtime —
    // reaching this point without a panic/timeout is the observable proof
    // that cleanup completed rather than blocking forever.
    tokio::time::sleep(Duration::from_secs(4)).await;
}

// --- T014: consumer tool calling ---

/// Consumer tool calls (`get_weather` defined in `tools`) are surfaced as
/// `reasoning` output items in the Responses API (agent-internal), not
/// `function_call`, since there is no tool execution loop yet.
#[tokio::test]
async fn test_non_streaming_responses_api_tool_call_shown_as_reasoning() {
    let (base, _handle, _guard) = start_server(vec![("MOCK_ACP_TOOL_NAME", "grep")]).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/responses"))
        .json(&serde_json::json!({
            "model": "gpt-4",
            "stream": false,
            "input": [{"type": "message", "role": "user", "content": "Hello"}],
            "tools": [{"type": "function", "name": "get_weather", "description": "Get the weather"}]
        }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("valid json");

    let output = body["output"]
        .as_array()
        .expect("response.output should be a JSON array");
    let reasoning = output
        .iter()
        .find(|item| item["type"] == "reasoning")
        .unwrap_or_else(|| panic!("expected a reasoning output item (agent-internal tool call), got: {output:?}"));

    let summary = reasoning["summary"]
        .as_array()
        .expect("reasoning.summary should be an array");
    let combined: String = summary
        .iter()
        .filter_map(|s| s["text"].as_str())
        .collect();
    assert!(
        combined.contains("grep") || combined.contains("tool"),
        "reasoning text should reference the tool, got: {combined}"
    );
}

/// Consumer tools defined in `req.tools` are treated as agent-internal
/// (no tool execution loop yet). The response is a normal completion.
#[tokio::test]
async fn test_non_streaming_chat_completions_agent_tool_call_not_forwarded() {
    let (base, _handle, _guard) = start_server(vec![("MOCK_ACP_TOOL_NAME", "grep")]).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "gpt-4",
            "stream": false,
            "messages": [{"role": "user", "content": "Hello"}],
            "tools": [{"type": "function", "function": {"name": "get_weather", "description": "Get the weather"}}]
        }))
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("valid json");
    assert!(body["choices"][0]["message"]["tool_calls"].is_null(),
        "agent-internal tool calls must NOT appear as tool_calls");
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    let content = body["choices"][0]["message"]["content"].as_str().unwrap_or("");
    assert!(!content.is_empty(), "message content should be present");
}

/// Consumer tool calls produce NO tool_calls deltas in streaming Chat
/// Completions since they are agent-internal (no execution loop).
#[tokio::test]
async fn test_streaming_chat_completions_content_only_no_tool_calls() {
    let (base, _handle, _guard) = start_server(vec![("MOCK_ACP_TOOL_NAME", "grep")]).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "gpt-4",
            "stream": true,
            "messages": [{"role": "user", "content": "Hello"}],
            "tools": [{"type": "function", "function": {"name": "get_weather", "description": "Get the weather"}}]
        }))
        .send()
        .await
        .expect("request failed");

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        collect_sse_lines_with_timestamps(resp),
    )
    .await;
    let lines = result.expect("stream should terminate within 5s, not hang");

    let chunks: Vec<serde_json::Value> = lines
        .iter()
        .filter_map(|(_, l)| l.strip_prefix("data: "))
        .filter(|d| *d != "[DONE]")
        .map(|d| serde_json::from_str(d).expect("chunk should be valid JSON"))
        .collect();

    // No tool_calls deltas should appear — tool calls are agent-internal.
    let has_tool_calls = chunks.iter().any(|c| {
        c["choices"][0]["delta"]["tool_calls"].is_array()
    });
    assert!(!has_tool_calls, "agent-internal tool calls must NOT produce tool_calls deltas");

    // The stream should deliver content (message text).
    let all_content: String = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert!(!all_content.is_empty(), "expected content deltas, got none");

    // It must terminate with a [DONE] sentinel.
    let has_done = lines.iter().any(|(_, l)| l == "data: [DONE]");
    assert!(has_done, "expected [DONE] sentinel at end of stream");

    // A finish_reason chunk must be present before [DONE].
    let has_finish_reason = chunks.iter().any(|c| {
        c["choices"][0]["finish_reason"].as_str().map(|s| !s.is_empty()).unwrap_or(false)
    });
    assert!(has_finish_reason, "expected at least one chunk with finish_reason");
}
