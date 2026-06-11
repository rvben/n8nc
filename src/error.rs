use std::process::ExitCode;

use serde::Serialize;
use serde_json::Value;

/// Structured error as defined by the clispec v0.2 envelope.
///
/// The envelope is always emitted as the last line of stderr so consumers can
/// extract it mechanically without parsing surrounding progress output.
#[derive(Debug, Clone)]
pub struct AppError {
    pub exit_code: u8,
    pub command: &'static str,
    /// Stable snake_case kind identifier (e.g. "auth", "not_found").
    pub kind: String,
    /// Legacy alias kept for callers that still reference `.code`.
    pub code: String,
    pub message: String,
    /// Human-readable remediation hint (also surfaced as `suggestion` by legacy callers).
    pub hint: Option<String>,
    /// Kept for callers that still reference `.suggestion`.
    pub suggestion: Option<String>,
    /// Extra JSON data attached to some error responses (not emitted in the clispec envelope).
    pub json_data: Option<Value>,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope<'a> {
    error: ErrorBody<'a>,
}

#[derive(Debug, Serialize)]
struct ErrorBody<'a> {
    kind: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<&'a str>,
}

impl AppError {
    pub fn new(
        exit_code: u8,
        command: &'static str,
        kind: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let kind = kind.into();
        let code = kind.clone();
        Self {
            exit_code,
            command,
            kind,
            code,
            message: message.into(),
            hint: None,
            suggestion: None,
            json_data: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        let h = hint.into();
        self.hint = Some(h.clone());
        self.suggestion = Some(h);
        self
    }

    pub fn with_suggestion(self, suggestion: impl Into<String>) -> Self {
        self.with_hint(suggestion)
    }

    pub fn with_json_data(mut self, data: Value) -> Self {
        self.json_data = Some(data);
        self
    }

    pub fn usage(command: &'static str, message: impl Into<String>) -> Self {
        Self::new(2, command, "usage", message)
    }

    pub fn config(command: &'static str, message: impl Into<String>) -> Self {
        Self::new(3, command, "config", message)
    }

    pub fn auth(command: &'static str, message: impl Into<String>) -> Self {
        Self::new(4, command, "auth", message)
    }

    pub fn network(command: &'static str, message: impl Into<String>) -> Self {
        Self::new(5, command, "network", message)
    }

    /// Build an API error. The `code` parameter is preserved on `self.code` for internal
    /// callers that branch on it; the clispec `kind` is always `"api"`.
    pub fn api(command: &'static str, code: impl Into<String>, message: impl Into<String>) -> Self {
        let mut err = Self::new(6, command, "api", message);
        err.code = code.into();
        err
    }

    pub fn validation(command: &'static str, message: impl Into<String>) -> Self {
        Self::new(10, command, "validation", message)
    }

    pub fn not_found(command: &'static str, message: impl Into<String>) -> Self {
        Self::new(11, command, "not_found", message)
    }

    pub fn conflict(command: &'static str, message: impl Into<String>) -> Self {
        Self::new(12, command, "conflict", message)
    }

    pub fn confirmation_required(command: &'static str, message: impl Into<String>) -> Self {
        Self::new(2, command, "confirmation_required", message)
    }

    /// Emit the structured error envelope as the last line of stderr and return the exit code.
    ///
    /// The envelope is always written to stderr regardless of the `json_output` flag
    /// because the spec requires a machine-extractable last line on stderr.
    pub fn emit_and_exit(&self, _json_output: bool) -> ExitCode {
        let effective_hint = self.hint.as_deref().or(self.suggestion.as_deref());
        let envelope = ErrorEnvelope {
            error: ErrorBody {
                kind: &self.kind,
                message: &self.message,
                hint: effective_hint,
            },
        };

        match serde_json::to_string(&envelope) {
            Ok(line) => eprintln!("{line}"),
            Err(_) => {
                eprintln!(
                    r#"{{"error":{{"kind":"{}","message":"{}"}}}}"#,
                    self.kind,
                    self.message.replace('"', "\\\""),
                );
            }
        }

        ExitCode::from(self.exit_code)
    }
}
