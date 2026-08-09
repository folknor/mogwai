// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `Refusal`, ported from `analysis/mnq_fit.py`'s `class Refusal(Exception)`:
//! every fail-closed input-contract violation the stream contract and
//! preflight raise. Refusals are the interface (task brief) - this type
//! carries the message only, matching the Python side's plain
//! `raise Refusal("...")` string payload.

use std::fmt;

#[derive(Debug)]
pub enum LabError {
    /// A fail-closed contract violation - the Python `Refusal` exception.
    Refusal(String),
    Io(std::io::Error),
    Json(serde_json::Error),
    /// A harness/environment failure whose historical CLI text is part of
    /// the artifact-writing contract.
    Harness(String),
}

impl LabError {
    pub fn refusal(msg: impl Into<String>) -> Self {
        LabError::Refusal(msg.into())
    }
}

impl fmt::Display for LabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LabError::Refusal(msg) => write!(f, "refusal: {msg}"),
            LabError::Io(e) => write!(f, "io error: {e}"),
            LabError::Json(e) => write!(f, "json error: {e}"),
            LabError::Harness(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for LabError {}

impl From<std::io::Error> for LabError {
    fn from(e: std::io::Error) -> Self {
        LabError::Io(e)
    }
}

impl From<serde_json::Error> for LabError {
    fn from(e: serde_json::Error) -> Self {
        LabError::Json(e)
    }
}

pub type LabResult<T> = Result<T, LabError>;
