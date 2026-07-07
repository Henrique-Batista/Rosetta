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
#[allow(dead_code)]
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
