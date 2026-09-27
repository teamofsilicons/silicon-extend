//! Every JSON body is `{"type": "<kind>", "data": {...}}`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Envelope<T> {
    #[serde(rename = "type")]
    pub kind: String,
    pub data: T,
}

impl<T> Envelope<T> {
    pub fn new(kind: impl Into<String>, data: T) -> Self {
        Self {
            kind: kind.into(),
            data,
        }
    }
}

/// A page of results.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}
