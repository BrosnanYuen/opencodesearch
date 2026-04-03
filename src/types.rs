use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Parsed and indexed chunk metadata stored across backends.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CodeChunk {
    pub id: String,
    pub path: String,
    pub snippet: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// Search result returned by semantic or keyword search.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchHit {
    pub path: String,
    pub snippet: String,
    pub start_line: i64,
    pub end_line: i64,
    pub score: f32,
    pub source: String,
}

/// Update notification passed from watchdog process to orchestrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateSignal {
    UpdateStart,
    UpdateEnd,
}

impl UpdateSignal {
    pub fn as_bytes(&self) -> &'static [u8] {
        match self {
            Self::UpdateStart => b"UPDATE_START",
            Self::UpdateEnd => b"UPDATE_END",
        }
    }

    pub fn parse(input: &[u8]) -> Option<Self> {
        // Trim trailing null bytes and whitespace from datagram payloads.
        let trimmed = input
            .iter()
            .copied()
            .take_while(|byte| *byte != 0)
            .collect::<Vec<u8>>();
        let s = String::from_utf8(trimmed).ok()?;
        match s.trim() {
            "UPDATE_START" => Some(Self::UpdateStart),
            "UPDATE_END" => Some(Self::UpdateEnd),
            _ => None,
        }
    }
}
