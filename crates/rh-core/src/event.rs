//! Typed events.
//!
//! Events are the extension points of the harness. In DeepSeek Harness
//! these are typed with TypeScript declaration merging; in Rust the type
//! system itself is the map: an event is any `Send + Sync + 'static`
//! value, and subscribers are keyed by its concrete type.

use std::sync::Arc;

/// Marker trait for an event payload.
///
/// Blanket-implemented for every `Send + Sync + 'static` type, so any
/// value — a session event, an agent event, a capability event — is an
/// event without ceremony.
pub trait Event: Send + Sync + 'static {}

impl<E: Send + Sync + 'static> Event for E {}

/// A synchronous event handler.
///
/// Handlers run inline on [`crate::Context::emit`]; use this for
/// observation (logging, telemetry, rendering). Long or async work should
/// be dispatched onto a runtime from inside the handler.
pub type EventHandler<E> = Arc<dyn Fn(&E) + Send + Sync>;
