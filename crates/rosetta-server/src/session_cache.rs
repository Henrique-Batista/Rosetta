use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rosetta_acp::client::AcpClient;
use tokio::sync::Mutex;
use tracing::warn;

struct CacheEntry {
    client: AcpClient,
    session_id: String,
    inserted_at: Instant,
}

/// TTL-based cache mapping a Responses API `response_id` to its live
/// `(AcpClient, session_id)` pair, so a follow-up request carrying tool
/// outputs can resume the same ACP session instead of spawning a new agent.
///
/// Expired entries are evicted lazily (on `insert`/`take`) and their
/// underlying ACP child process is shut down asynchronously.
pub struct SessionCache {
    entries: Arc<Mutex<HashMap<String, CacheEntry>>>,
    ttl: Duration,
}

impl SessionCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            ttl,
        }
    }

    pub async fn insert(&self, response_id: String, client: AcpClient, session_id: String) {
        let mut map = self.entries.lock().await;
        self.evict_expired_locked(&mut map);
        map.insert(
            response_id,
            CacheEntry {
                client,
                session_id,
                inserted_at: Instant::now(),
            },
        );
    }

    pub async fn take(&self, response_id: &str) -> Option<(AcpClient, String)> {
        let mut map = self.entries.lock().await;
        self.evict_expired_locked(&mut map);
        map.remove(response_id).map(|e| (e.client, e.session_id))
    }

    fn evict_expired_locked(&self, map: &mut HashMap<String, CacheEntry>) {
        let now = Instant::now();
        let expired: Vec<String> = map
            .iter()
            .filter(|(_, e)| now.duration_since(e.inserted_at) > self.ttl)
            .map(|(k, _)| k.clone())
            .collect();
        for key in expired {
            if let Some(entry) = map.remove(&key) {
                warn!(response_id = %key, "Session cache entry expired");
                tokio::spawn(async move {
                    let _ = entry.client.shutdown().await;
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosetta_acp::AcpTransport;
    use rosetta_acp::client::AcpClient;

    fn mock_script_path() -> &'static str {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../rosetta-acp/tests/fixtures/mock_acp.py"
        )
    }

    async fn spawn_test_client() -> (AcpClient, String) {
        let transport = AcpTransport::new_with_env("python3", &[mock_script_path()], &[])
            .await
            .expect("spawn mock agent");
        let mut client = AcpClient::new(transport);
        client.initialize().await.expect("initialize");
        let session = client.new_session("/tmp", None).await.expect("new_session");
        (client, session.session_id)
    }

    #[tokio::test]
    async fn test_session_cache_insert_take() {
        let cache = SessionCache::new(Duration::from_secs(60));
        let (client, session_id) = spawn_test_client().await;
        let response_id = "resp_test_insert_take".to_string();

        cache
            .insert(response_id.clone(), client, session_id.clone())
            .await;

        let taken = cache.take(&response_id).await;
        assert!(taken.is_some(), "entry should be retrievable right after insert");
        let (client, taken_session_id) = taken.unwrap();
        assert_eq!(taken_session_id, session_id);

        // A second take must return None — take() removes the entry.
        assert!(cache.take(&response_id).await.is_none());

        client.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn test_session_cache_ttl_eviction() {
        let cache = SessionCache::new(Duration::from_millis(20));
        let (client, session_id) = spawn_test_client().await;
        let response_id = "resp_test_ttl_eviction".to_string();

        cache.insert(response_id.clone(), client, session_id).await;

        tokio::time::sleep(Duration::from_millis(100)).await;

        // take() runs eviction before lookup, so the expired entry is gone
        // (its underlying client is shut down asynchronously via tokio::spawn).
        let taken = cache.take(&response_id).await;
        assert!(taken.is_none(), "expired entry should have been evicted");
    }
}
