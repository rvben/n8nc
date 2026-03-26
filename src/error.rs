use std::process::ExitCode;

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct AppError {
    pub exit_code: u8,
    pub command: &'static str,
    pub code: String,
    pub message: String,
    pub suggestion: Option<String>,
    pub json_data: Option<Value>,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope<'a> {
    ok: bool,
    command: &'a str,
    version: &'a str,
    contract_version: u32,
    error: ErrorBody<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<&'a Value>,
}

#[derive(Debug, Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggestion: Option<&'a str>,
}

impl AppError {
    pub fn new(
        exit_code: u8,
        command: &'static str,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            exit_code,
            command,
            code: code.into(),
            message: message.into(),
            suggestion: None,
            json_data: None,
        }
    }

    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }

    pub fn with_json_data(mut self, data: Value) -> Self {
        self.json_data = Some(data);
        self
    }

    pub fn usage(command: &'static str, message: impl Into<String>) -> Self {
        Self::new(2, command, "usage.invalid", message)
    }

    pub fn config(command: &'static str, message: impl Into<String>) -> Self {
        Self::new(3, command, "config.invalid", message)
    }

    pub fn auth(command: &'static str, message: impl Into<String>) -> Self {
        Self::new(4, command, "auth.failed", message)
    }

    pub fn network(command: &'static str, message: impl Into<String>) -> Self {
        Self::new(5, command, "network.failed", message)
    }

    pub fn api(command: &'static str, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(6, command, code, message)
    }

    pub fn validation(command: &'static str, message: impl Into<String>) -> Self {
        Self::new(10, command, "validation.failed", message)
    }

    pub fn not_found(command: &'static str, message: impl Into<String>) -> Self {
        Self::new(11, command, "resource.not_found", message)
    }

    pub fn conflict(command: &'static str, message: impl Into<String>) -> Self {
        Self::new(12, command, "conflict.remote_changed", message)
    }

    pub fn emit_and_exit(&self, json_output: bool) -> ExitCode {
        let envelope = ErrorEnvelope {
            ok: false,
            command: self.command,
            version: env!("CARGO_PKG_VERSION"),
            contract_version: 1,
            error: ErrorBody {
                code: &self.code,
                message: &self.message,
                suggestion: self.suggestion.as_deref(),
            },
            data: self.json_data.as_ref(),
        };

        if json_output {
            match serde_json::to_string_pretty(&envelope) {
                Ok(serialized) => println!("{serialized}"),
                Err(_) => {
                    println!(
                        r#"{{"ok":false,"command":"{}","version":"{}","contract_version":1,"error":{{"code":"{}","message":"{}"}}}}"#,
                        self.command,
                        env!("CARGO_PKG_VERSION"),
                        self.code,
                        self.message.replace('"', "\\\""),
                    );
                }
            }
        } else {
            eprintln!("Error: {}", self.message);
            if let Some(suggestion) = &self.suggestion {
                eprintln!("{suggestion}");
            }
        }

        ExitCode::from(self.exit_code)
    }
}
