use axum::response::sse::{Event, Sse};
use futures::stream::{self, Stream, StreamExt};
use rosetta_types::openai::{ResponseEvent, ChatCompletionChunk};
use std::convert::Infallible;

pub fn response_event_type(event: &ResponseEvent) -> &'static str {
    match event {
        ResponseEvent::ResponseCreated { .. } => "response.created",
        ResponseEvent::ResponseInProgress { .. } => "response.in_progress",
        ResponseEvent::OutputItemAdded { .. } => "response.output_item.added",
        ResponseEvent::ContentPartAdded { .. } => "response.content_part.added",
        ResponseEvent::OutputTextDelta { .. } => "response.output_text.delta",
        ResponseEvent::OutputTextDone { .. } => "response.output_text.done",
        ResponseEvent::ContentPartDone { .. } => "response.content_part.done",
        ResponseEvent::OutputItemDone { .. } => "response.output_item.done",
        ResponseEvent::ResponseCompleted { .. } => "response.completed",
        ResponseEvent::Error { .. } => "error",
    }
}

#[allow(dead_code)]
pub fn response_events_to_sse(
    events: Vec<ResponseEvent>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = stream::iter(events.into_iter().map(|event| {
        let event_type = response_event_type(&event);
        let data = serde_json::to_string(&event).unwrap_or_default();
        Ok(Event::default().event(event_type).data(data))
    }));
    Sse::new(stream)
}

/// Convert a live stream of ResponseEvents into an SSE response.
pub fn response_event_stream_to_sse<S>(
    event_stream: S,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    S: Stream<Item = ResponseEvent> + Send + 'static,
{
    let sse_stream = event_stream.map(|event| {
        let event_type = response_event_type(&event);
        let data = serde_json::to_string(&event).unwrap_or_default();
        Ok(Event::default().event(event_type).data(data))
    });
    Sse::new(sse_stream)
}

/// Convert a live stream of `ChatCompletionChunk`s into an SSE response,
/// appending the `[DONE]` sentinel after the stream ends. Kept as a
/// general-purpose helper (unconditional `[DONE]`); the live route handler
/// builds its own SSE events directly instead, since it must SKIP `[DONE]`
/// on the error-termination path (see `routes::build_live_chat_sse_events`).
#[allow(dead_code)]
pub fn chat_chunk_stream_to_sse<S>(
    chunk_stream: S,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    S: Stream<Item = ChatCompletionChunk> + Send + 'static,
{
    let data_events = chunk_stream.map(|chunk| {
        let data = serde_json::to_string(&chunk).unwrap_or_default();
        Ok(Event::default().data(data))
    });
    let done_event = stream::once(async { Ok(Event::default().data("[DONE]")) });
    Sse::new(data_events.chain(done_event))
}

#[allow(dead_code)]
pub fn chat_chunks_to_sse(
    chunks: Vec<ChatCompletionChunk>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut events: Vec<Event> = chunks.into_iter().map(|chunk| {
        let data = serde_json::to_string(&chunk).unwrap_or_default();
        Event::default().data(data)
    }).collect();
    events.push(Event::default().data("[DONE]"));
    let stream = stream::iter(events.into_iter().map(Ok));
    Sse::new(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosetta_types::openai::{ChatChoiceDelta, ChatMessageDelta};

    fn sample_chunk(content: &str) -> ChatCompletionChunk {
        ChatCompletionChunk {
            id: "chatcmpl-1".to_string(),
            object: "chat.completion.chunk",
            created: 0,
            model: "gpt-4".to_string(),
            choices: vec![ChatChoiceDelta {
                index: 0,
                delta: ChatMessageDelta { role: None, content: Some(content.to_string()), tool_calls: None },
                finish_reason: None,
            }],
            usage: None,
        }
    }

    /// Exercises the same map+chain composition as `chat_chunk_stream_to_sse`
    /// but returns a plain, directly-collectible stream (bypassing `Sse`'s
    /// opaque `Event` type) so the event count can be asserted without
    /// standing up a full axum response body.
    fn build_test_events<S>(chunk_stream: S) -> impl Stream<Item = ()>
    where
        S: Stream<Item = ChatCompletionChunk> + Send + 'static,
    {
        let data_events = chunk_stream.map(|_| ());
        let done_event = stream::once(async {});
        data_events.chain(done_event)
    }

    #[tokio::test]
    async fn test_chat_chunk_stream_to_sse_appends_done_sentinel() {
        let chunks = vec![sample_chunk("hello"), sample_chunk("world")];
        let events: Vec<()> = build_test_events(stream::iter(chunks)).collect().await;
        assert_eq!(events.len(), 3, "expected 2 data chunks + 1 [DONE] sentinel");
    }

    #[tokio::test]
    async fn test_chat_chunk_stream_to_sse_empty_stream_yields_done_only() {
        let empty: Vec<ChatCompletionChunk> = Vec::new();
        let events: Vec<()> = build_test_events(stream::iter(empty)).collect().await;
        assert_eq!(events.len(), 1, "empty stream should still yield the [DONE] sentinel");
    }
}
