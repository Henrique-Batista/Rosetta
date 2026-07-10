use futures::Stream;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::{debug, error, trace, warn};

use rosetta_types::acp::{
    ContentBlock, InitializeRequest, InitializeResponse, JsonRpcNotification,
    JsonRpcRequest, JsonRpcResponse, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, SessionUpdate, SetConfigOptionRequest, SetConfigOptionResponse,
};

use crate::{AcpError, AcpTransport};

/// One item yielded by [`AcpClient::send_prompt_streaming`].
#[derive(Debug, Clone)]
pub enum AcpStreamItem {
    Update(SessionUpdate),
    Completed,
    Disconnected,
}

/// High-level ACP client that wraps [`AcpTransport`] and speaks JSON-RPC 2.0.
pub struct AcpClient {
    transport: AcpTransport,
    request_id: u64,
    pending_updates: Vec<SessionUpdate>,
}

impl AcpClient {
    /// Create a new client from an existing transport.
    #[must_use]
    pub fn new(transport: AcpTransport) -> Self {
        Self {
            transport,
            request_id: 0,
            pending_updates: Vec::new(),
        }
    }

    /// Create a new client spawning a fresh transport with extra env vars.
    pub async fn spawn(program: &str, args: &[&str], env_vars: &[(String, String)]) -> Result<Self, AcpError> {
        let transport = AcpTransport::new_with_env(program, args, env_vars).await?;
        Ok(Self::new(transport))
    }

    fn next_id(&mut self) -> u64 {
        self.request_id += 1;
        self.request_id
    }

    /// Send the `initialize` request and wait for the matching response.
    pub async fn initialize(&mut self) -> Result<InitializeResponse, AcpError> {
        let id = self.next_id();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: "initialize".to_string(),
            params: InitializeRequest {
                protocol_version: 1,
            },
        };
        let msg = serde_json::to_string(&req).map_err(AcpError::Json)?;
        debug!(id, "Sending initialize request");
        self.transport.send_message(&msg).await?;

        let resp = self.read_response::<InitializeResponse>(id).await?;
        debug!(id, "Received initialize response");
        Ok(resp)
    }

    /// Send the `session/new` request and return the full response.
    /// `mcp_servers` is an optional list of MCP server configurations to pass to the agent.
    pub async fn new_session(
        &mut self,
        cwd: &str,
        mcp_servers: Option<Vec<serde_json::Value>>,
    ) -> Result<NewSessionResponse, AcpError> {
        let id = self.next_id();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: "session/new".to_string(),
            params: NewSessionRequest {
                cwd: cwd.to_string(),
                mcp_servers,
            },
        };
        let msg = serde_json::to_string(&req).map_err(AcpError::Json)?;
        debug!(id, cwd, "Sending new_session request");
        self.transport.send_message(&msg).await?;

        let resp = self.read_response::<NewSessionResponse>(id).await?;
        debug!(id, session_id = %resp.session_id, "Received new_session response");
        Ok(resp)
    }

    /// Send the `session/prompt` request and return a stream of `session/update`
    /// notifications. The stream ends when the matching prompt response arrives.
    /// The caller must drop the stream before calling other &mut self methods.
    ///
    /// Each yielded [`AcpStreamItem`] distinguishes normal completion
    /// (`Completed`, once the matching `PromptResponse` arrives) from an
    /// abnormal disconnect (`Disconnected`, transport closed/errored without
    /// a matching response) — callers that need to tell these apart (e.g. to
    /// emit an identifiable error signal) can match on the variant instead of
    /// having both paths look like a plain stream end.
    pub fn send_prompt_streaming<'a>(
        &'a mut self,
        session_id: &'a str,
        prompt: Vec<ContentBlock>,
    ) -> impl Stream<Item = AcpStreamItem> + 'a {
        let id = self.next_id();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: "session/prompt".to_string(),
            params: PromptRequest {
                session_id: session_id.to_string(),
                prompt,
            },
        };
        async_stream::stream! {
            let msg = serde_json::to_string(&req).unwrap_or_default();
            if let Err(e) = self.transport.send_message(&msg).await {
                error!(error = %e, "Failed to send prompt in streaming");
                yield AcpStreamItem::Disconnected;
                return;
            }
            loop {
                match self.transport.read_line().await {
                    Ok(Some(line)) => {
                        if let Some(update) = self.try_parse_notification::<SessionUpdate>(&line) {
                            if update.session_id == session_id {
                                yield AcpStreamItem::Update(update);
                            }
                            continue;
                        }
                        // Prompt response arrived — stream is done normally.
                        // The response's `usage` field is intentionally not
                        // parsed/propagated here (out of scope, stays
                        // hard-coded per the project roadmap).
                        let _ = self.try_parse_response::<PromptResponse>(&line, id);
                        yield AcpStreamItem::Completed;
                        return;
                    }
                    Ok(None) | Err(_) => {
                        yield AcpStreamItem::Disconnected;
                        return;
                    }
                }
            }
        }
    }

    /// Send the `session/prompt` request and collect all `session/update` notifications
    /// until the matching prompt response arrives.
    pub async fn send_prompt(
        &mut self,
        session_id: &str,
        prompt: Vec<ContentBlock>,
    ) -> Result<PromptResponse, AcpError> {
        let id = self.next_id();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: "session/prompt".to_string(),
            params: PromptRequest {
                session_id: session_id.to_string(),
                prompt,
            },
        };
        let msg = serde_json::to_string(&req).map_err(AcpError::Json)?;
        debug!(id, session_id, "Sending prompt request");
        self.transport.send_message(&msg).await?;

        loop {
            match self.transport.read_line().await? {
                None => return Err(AcpError::Disconnected),
                Some(line) => {
                    trace!(line, "Received line after prompt");

                    if let Some(update) = self.try_parse_notification::<SessionUpdate>(&line) {
                        if update.session_id == session_id {
                            let update_type = update.body.get("updateType")
                                .and_then(|v| v.as_str())
                                .or_else(|| update.body.get("update").and_then(|u| u.get("sessionUpdate")).and_then(|v| v.as_str()))
                                .unwrap_or("unknown");
                            debug!(update_type, "Buffered session update");
                            self.pending_updates.push(update);
                        } else {
                            warn!(
                                received = %update.session_id,
                                expected = %session_id,
                                "Ignoring update for different session"
                            );
                        }
                        continue;
                    }

                    match self.try_parse_response::<PromptResponse>(&line, id)? {
                        Some(result) => {
                            debug!(id, "Received prompt response");
                            return Ok(result);
                        }
                        None => continue,
                    }
                }
            }
        }
    }

    /// Return buffered session updates. If the buffer is empty, read from the
    /// transport until a non-notification line is encountered.
    pub async fn read_updates(&mut self) -> Result<Vec<SessionUpdate>, AcpError> {
        if !self.pending_updates.is_empty() {
            return Ok(std::mem::take(&mut self.pending_updates));
        }

        let mut updates = Vec::new();
        loop {
            match self.transport.read_line().await? {
                None => return Err(AcpError::Disconnected),
                Some(line) => {
                    trace!(line, "Received line in read_updates");
                    if let Some(update) = self.try_parse_notification::<SessionUpdate>(&line) {
                        updates.push(update);
                    } else {
                        // Not a notification — stop reading updates.
                        break;
                    }
                }
            }
        }
        Ok(updates)
    }

    /// Shut down the underlying transport.
    pub async fn shutdown(self) -> Result<(), AcpError> {
        self.transport.shutdown().await
    }

    /// Send the `session/set_config_option` request.
    pub async fn set_config_option(
        &mut self,
        session_id: &str,
        config_id: &str,
        value: serde_json::Value,
    ) -> Result<SetConfigOptionResponse, AcpError> {
        let id = self.next_id();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: "session/set_config_option".to_string(),
            params: SetConfigOptionRequest {
                session_id: session_id.to_string(),
                config_id: config_id.to_string(),
                value,
            },
        };
        let msg = serde_json::to_string(&req).map_err(AcpError::Json)?;
        debug!(id, session_id, config_id, "Sending set_config_option request");
        self.transport.send_message(&msg).await?;

        let resp = self.read_response::<SetConfigOptionResponse>(id).await?;
        debug!(id, session_id, config_id, "Received set_config_option response");
        Ok(resp)
    }

    /// Send the `session/close` request.
    pub async fn close_session(&mut self, session_id: &str) -> Result<(), AcpError> {
        let id = self.next_id();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: "session/close".to_string(),
            params: serde_json::json!({ "sessionId": session_id }),
        };
        let msg = serde_json::to_string(&req).map_err(AcpError::Json)?;
        debug!(id, session_id, "Sending close_session request");
        self.transport.send_message(&msg).await?;

        let _resp = self.read_response::<Value>(id).await?;
        debug!(id, session_id, "Received close_session response");
        Ok(())
    }

    /// Read lines from the transport until a response with the expected id is found.
    /// Notifications encountered along the way are buffered.
    async fn read_response<T: DeserializeOwned>(&mut self, expected_id: u64) -> Result<T, AcpError> {
        loop {
            match self.transport.read_line().await? {
                None => return Err(AcpError::Disconnected),
                Some(line) => {
                    trace!(line, "Received line in read_response");

                    if let Some(update) = self.try_parse_notification::<SessionUpdate>(&line) {
                        self.pending_updates.push(update);
                        continue;
                    }

                    match self.try_parse_response::<T>(&line, expected_id)? {
                        Some(result) => return Ok(result),
                        None => continue,
                    }
                }
            }
        }
    }

    /// Attempt to parse a line as a JSON-RPC notification (no `id` field).
    fn try_parse_notification<T: DeserializeOwned>(&self, line: &str) -> Option<T> {
        let raw: Value = serde_json::from_str(line).ok()?;
        if raw.get("id").is_some() {
            return None;
        }
        let notification: JsonRpcNotification<T> = serde_json::from_str(line).ok()?;
        Some(notification.params)
    }

    /// Attempt to parse a line as a JSON-RPC response with the expected id.
    /// Returns `Ok(None)` when the id does not match or the line is not a response.
    fn try_parse_response<T: DeserializeOwned>(
        &self,
        line: &str,
        expected_id: u64,
    ) -> Result<Option<T>, AcpError> {
        let raw: Value = serde_json::from_str(line).map_err(AcpError::Json)?;
        let Some(id) = raw.get("id").and_then(|v| v.as_u64()) else {
            return Ok(None); // Likely a notification.
        };
        if id != expected_id {
            return Ok(None);
        }

        let resp: JsonRpcResponse<T> = serde_json::from_str(line).map_err(AcpError::Json)?;
        if let Some(err) = resp.error {
            error!(id, code = err.code, message = %err.message, "JSON-RPC error");
            return Err(AcpError::Protocol {
                message: format!("JSON-RPC error {}: {}", err.code, err.message),
            });
        }
        Ok(resp.result)
    }
}
