//! Minimal monotonic id generation (no external uuid dependency).

use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Kinds of identity the log uses; only affects the id prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdKind {
    Session,
    Message,
    Turn,
    Step,
    ToolCall,
}

impl IdKind {
    fn prefix(self) -> &'static str {
        match self {
            IdKind::Session => "sess",
            IdKind::Message => "msg",
            IdKind::Turn => "turn",
            IdKind::Step => "step",
            IdKind::ToolCall => "call",
        }
    }
}

/// Generate the next id of the given kind.
pub fn next_id(kind: IdKind) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{n}", kind.prefix())
}
