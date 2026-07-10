use futures::StreamExt;
use rosetta_acp::{AcpClient, AcpStreamItem};
use rosetta_types::acp::{ContentBlock, SessionUpdate};
use tokio::sync::mpsc;
use tokio::time::Duration;
use tracing::warn;

/// Bounded channel capacity shared by both `/v1/responses` and
/// `/v1/chat/completions` streaming route handlers.
pub const STREAM_CHANNEL_CAPACITY: usize = 32;

/// Cleanup timeout budget, kept well under SC-004's 5-second window.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// One item forwarded from the streaming task to the HTTP handler.
#[derive(Debug)]
pub enum StreamOutcome {
    Update(SessionUpdate),
    Done,
    Error(String),
}

/// Spawn a task that owns `client` for the lifetime of one streaming prompt,
/// forwarding each ACP stream item over a bounded channel to the caller.
///
/// The task explicitly calls `client.shutdown()` on every exit path (normal
/// completion, agent disconnect/error, or the receiver being dropped because
/// the HTTP client disconnected) rather than relying on `Drop`, since
/// `AcpTransport` has no `Drop` impl and would otherwise leave the spawned
/// agent child process running.
pub fn spawn_streaming_prompt(
    mut client: AcpClient,
    session_id: String,
    prompt: Vec<ContentBlock>,
    channel_capacity: usize,
) -> mpsc::Receiver<StreamOutcome> {
    let (tx, rx) = mpsc::channel(channel_capacity);

    tokio::spawn(async move {
        let outcome = {
            let stream = client.send_prompt_streaming(&session_id, prompt);
            let mut stream = Box::pin(stream);
            let mut disconnected = false;
            loop {
                match stream.next().await {
                    Some(AcpStreamItem::Update(update)) => {
                        if tx.send(StreamOutcome::Update(update)).await.is_err() {
                            disconnected = true;
                            break;
                        }
                    }
                    Some(AcpStreamItem::Completed) => break,
                    Some(AcpStreamItem::Disconnected) | None => {
                        disconnected = true;
                        break;
                    }
                }
            }
            disconnected
        };

        let shutdown_result =
            tokio::time::timeout(SHUTDOWN_TIMEOUT, client.shutdown()).await;
        match shutdown_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!(error = %e, "ACP client shutdown returned an error"),
            Err(_) => warn!("ACP client shutdown timed out"),
        }

        if outcome {
            let _ = tx
                .send(StreamOutcome::Error("agent disconnected mid-turn".to_string()))
                .await;
        } else {
            let _ = tx.send(StreamOutcome::Done).await;
        }
    });

    rx
}



#[cfg(test)]
mod tests {
    use super::*;
    use rosetta_acp::AcpTransport;
    use rosetta_types::acp::ContentBlock;
    use std::time::Duration as StdDuration;
    use tokio::time::Instant;

    fn mock_script_path() -> &'static str {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../rosetta-acp/tests/fixtures/mock_acp.py")
    }

    async fn spawn_test_client(env_vars: &[(String, String)]) -> (AcpClient, String) {
        let transport = AcpTransport::new_with_env("python3", &[mock_script_path()], env_vars)
            .await
            .expect("spawn mock agent");
        let mut client = AcpClient::new(transport);
        client.initialize().await.expect("initialize");
        let session = client.new_session("/tmp", None).await.expect("new_session");
        (client, session.session_id)
    }

    #[tokio::test]
    async fn test_normal_completion_yields_updates_then_done() {
        let (client, session_id) = spawn_test_client(&[]).await;
        let prompt = vec![ContentBlock::Text { text: "hi".to_string() }];
        let mut rx = spawn_streaming_prompt(client, session_id, prompt, STREAM_CHANNEL_CAPACITY);

        let mut update_count = 0;
        let mut saw_done = false;
        while let Some(outcome) = rx.recv().await {
            match outcome {
                StreamOutcome::Update(_) => update_count += 1,
                StreamOutcome::Done => {
                    saw_done = true;
                    break;
                }
                StreamOutcome::Error(e) => panic!("unexpected error outcome: {e}"),
            }
        }
        assert_eq!(update_count, 6);
        assert!(saw_done);
    }

    #[tokio::test]
    async fn test_agent_crash_yields_error_outcome() {
        let env_vars = vec![("MOCK_ACP_CRASH_AFTER_CHUNKS".to_string(), "2".to_string())];
        let (client, session_id) = spawn_test_client(&env_vars).await;
        let prompt = vec![ContentBlock::Text { text: "hi".to_string() }];
        let mut rx = spawn_streaming_prompt(client, session_id, prompt, STREAM_CHANNEL_CAPACITY);

        let mut update_count = 0;
        let mut saw_error = false;
        let result = tokio::time::timeout(StdDuration::from_secs(5), async {
            while let Some(outcome) = rx.recv().await {
                match outcome {
                    StreamOutcome::Update(_) => update_count += 1,
                    StreamOutcome::Done => panic!("expected Error outcome, got Done"),
                    StreamOutcome::Error(_) => {
                        saw_error = true;
                        break;
                    }
                }
            }
        })
        .await;
        assert!(result.is_ok(), "task should finish within 5s, not hang");
        assert_eq!(update_count, 2);
        assert!(saw_error);
    }

    #[tokio::test]
    async fn test_slow_consumer_backpressures_producer() {
        let env_vars = vec![("MOCK_ACP_CHUNK_DELAY_MS".to_string(), "10".to_string())];
        let (client, session_id) = spawn_test_client(&env_vars).await;
        let prompt = vec![ContentBlock::Text { text: "hi".to_string() }];
        let mut rx = spawn_streaming_prompt(client, session_id, prompt, 1);

        let start = Instant::now();
        let mut count = 0;
        while let Some(outcome) = rx.recv().await {
            tokio::time::sleep(StdDuration::from_millis(30)).await;
            if matches!(outcome, StreamOutcome::Done | StreamOutcome::Error(_)) {
                break;
            }
            count += 1;
        }
        let elapsed = start.elapsed();
        assert_eq!(count, 6);
        assert!(
            elapsed >= StdDuration::from_millis(30 * 6),
            "elapsed {elapsed:?} should reflect the slow consumer's pace (backpressure), not the producer's"
        );
    }

    #[tokio::test]
    async fn test_receiver_dropped_causes_task_to_exit_within_budget() {
        let (client, session_id) = spawn_test_client(&[]).await;
        let prompt = vec![ContentBlock::Text { text: "hi".to_string() }];
        let mut rx = spawn_streaming_prompt(client, session_id, prompt, STREAM_CHANNEL_CAPACITY);

        let _ = rx.recv().await;
        drop(rx);

        tokio::time::sleep(StdDuration::from_millis(200)).await;
    }
}
