//! Error types for Capsule operations
//!
//! This module defines error types used across the capsule_types module.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Capsule-specific errors for UARC V1.1.0 Capsule Manifest
#[derive(Debug, Clone, Error)]
pub enum CapsuleError {
    /// Failed to parse manifest file
    #[error("Parse error: {0}")]
    ParseError(String),
    /// Failed to serialize manifest
    #[error("Serialize error: {0}")]
    SerializeError(String),
    /// IO error (file not found, etc.)
    #[error("IO error: {0}")]
    IoError(String),
    /// Invalid memory string format (e.g., "6GB")
    #[error("Invalid memory string: {0}")]
    InvalidMemoryString(String),
    /// Validation failed
    #[error("Validation error: {0}")]
    ValidationError(String),
}

/// Structured error used across UARC components.
#[derive(Debug)]
pub struct ManifestError {
    code: String,
    message: String,
    hint: Option<String>,
    docs: Option<String>,
    context: Option<serde_json::Value>,
}

impl ManifestError {
    /// Create a new error with the given code and message.
    pub fn new<C, M>(code: C, message: M) -> Self
    where
        C: Into<String>,
        M: Into<String>,
    {
        Self {
            code: code.into(),
            message: message.into(),
            hint: None,
            docs: None,
            context: None,
        }
    }

    pub fn with_hint<H>(mut self, hint: H) -> Self
    where
        H: Into<String>,
    {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_docs<D>(mut self, docs: D) -> Self
    where
        D: Into<String>,
    {
        self.docs = Some(docs.into());
        self
    }

    pub fn with_context(mut self, context: serde_json::Value) -> Self {
        self.context = Some(context);
        self
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn into_wire(self) -> ManifestErrorWire {
        ManifestErrorWire {
            code: self.code,
            message: self.message,
            hint: self.hint,
            docs: self.docs,
            context: self.context,
        }
    }
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, " (hint: {hint})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ManifestError {}

/// JSON-wire representation of the structured error used by the engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestErrorWire {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}
