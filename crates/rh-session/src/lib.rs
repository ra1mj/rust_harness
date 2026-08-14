//! rh-session — the append-only session log.
//!
//! Ports DeepSeek Harness's central invariant: **model-visible means
//! logged**. The [`Session`] is an append-only stream of
//! [`SessionEvent`]s that is the *source of truth* for everything the model
//! sees: [`Session::derive_messages`] projects model history from the log,
//! and nothing reaches a model request except through that projection.
//! Forking, resuming, transcripts, and telemetry all derive from the same
//! stream.
//!
//! Appending a durable event also broadcasts it on the shared
//! [`Context`](rh_core::Context), so observers (UI, telemetry) read the
//! same facts the model does.

mod ids;
mod session;

pub use ids::{next_id, IdKind};
pub use session::{
    ContentBlock, Message, Role, Session, SessionEvent, SessionPlugin, SessionStore,
};

/// Identity of a session.
pub type SessionId = String;
/// Identity of a message.
pub type MessageId = String;
/// Identity of a turn.
pub type TurnId = String;
/// Identity of a step.
pub type StepId = String;
