use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LintSeverity {
    Off,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct LintDiagnostic {
    pub severity: LintSeverity,
    pub rule: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Built-in rules with defaults
// ---------------------------------------------------------------------------

struct RuleDefinition {
    id: &'static str,
    default: LintSeverity,
    check: fn(&Value, LintSeverity) -> Vec<LintDiagnostic>,
}

const RULES: &[RuleDefinition] = &[
    RuleDefinition {
        id: "no-hardcoded-urls",
        default: LintSeverity::Warn,
        check: check_no_hardcoded_urls,
    },
    RuleDefinition {
        id: "no-disabled-nodes",
        default: LintSeverity::Warn,
        check: check_no_disabled_nodes,
    },
    RuleDefinition {
        id: "require-error-handler",
        default: LintSeverity::Off,
        check: check_require_error_handler,
    },
    RuleDefinition {
        id: "no-default-names",
        default: LintSeverity::Warn,
        check: check_no_default_names,
    },
    RuleDefinition {
        id: "no-empty-expressions",
        default: LintSeverity::Warn,
        check: check_no_empty_expressions,
    },
];

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn lint_workflow(
    workflow: &Value,
    config: &BTreeMap<String, String>,
    only_rule: Option<&str>,
) -> Vec<LintDiagnostic> {
    let mut diagnostics = Vec::new();

    for rule in RULES {
        if let Some(only) = only_rule
            && rule.id != only
        {
            continue;
        }

        let severity = config
            .get(rule.id)
            .and_then(|value| parse_severity(value))
            .unwrap_or(rule.default);

        if severity == LintSeverity::Off {
            continue;
        }

        diagnostics.extend((rule.check)(workflow, severity));
    }

    diagnostics
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_severity(value: &str) -> Option<LintSeverity> {
    match value.to_ascii_lowercase().as_str() {
        "off" => Some(LintSeverity::Off),
        "warn" => Some(LintSeverity::Warn),
        "error" => Some(LintSeverity::Error),
        _ => None,
    }
}

fn nodes(workflow: &Value) -> impl Iterator<Item = &Value> {
    workflow
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn node_name(node: &Value) -> String {
    node.get("name")
        .and_then(Value::as_str)
        .unwrap_or("<unnamed>")
        .to_string()
}

fn node_type(node: &Value) -> Option<&str> {
    node.get("type").and_then(Value::as_str)
}

// ---------------------------------------------------------------------------
// Rule: no-hardcoded-urls
// ---------------------------------------------------------------------------

fn check_no_hardcoded_urls(workflow: &Value, severity: LintSeverity) -> Vec<LintDiagnostic> {
    let mut out = Vec::new();

    for node in nodes(workflow) {
        let Some(node_type_str) = node_type(node) else {
            continue;
        };
        if !node_type_str.contains("httpRequest") {
            continue;
        }
        let name = node_name(node);
        let params = node.get("parameters");

        // Check parameters.url
        if let Some(url) = params.and_then(|p| p.get("url")).and_then(Value::as_str)
            && !url.starts_with("={{")
        {
            out.push(LintDiagnostic {
                severity,
                rule: "no-hardcoded-urls".to_string(),
                node: Some(name.clone()),
                message: format!(
                    "Hardcoded URL `{url}` — consider using an expression or environment variable."
                ),
            });
        }

        // Check parameters.options.url
        if let Some(url) = params
            .and_then(|p| p.get("options"))
            .and_then(|o| o.get("url"))
            .and_then(Value::as_str)
            && !url.starts_with("={{")
        {
            out.push(LintDiagnostic {
                severity,
                rule: "no-hardcoded-urls".to_string(),
                node: Some(name.clone()),
                message: format!("Hardcoded URL `{url}` in options — consider using an expression or environment variable."),
            });
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Rule: no-disabled-nodes
// ---------------------------------------------------------------------------

fn check_no_disabled_nodes(workflow: &Value, severity: LintSeverity) -> Vec<LintDiagnostic> {
    let mut out = Vec::new();

    for node in nodes(workflow) {
        if node.get("disabled") == Some(&Value::Bool(true)) {
            out.push(LintDiagnostic {
                severity,
                rule: "no-disabled-nodes".to_string(),
                node: Some(node_name(node)),
                message: "Node is disabled.".to_string(),
            });
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Rule: require-error-handler
// ---------------------------------------------------------------------------

fn check_require_error_handler(workflow: &Value, severity: LintSeverity) -> Vec<LintDiagnostic> {
    let has_error_trigger =
        nodes(workflow).any(|node| node_type(node).is_some_and(|t| t.contains("errorTrigger")));

    if has_error_trigger {
        Vec::new()
    } else {
        vec![LintDiagnostic {
            severity,
            rule: "require-error-handler".to_string(),
            node: None,
            message: "Workflow has no error-trigger node.".to_string(),
        }]
    }
}

// ---------------------------------------------------------------------------
// Rule: no-default-names
// ---------------------------------------------------------------------------

fn type_to_display_name(type_str: &str) -> String {
    // Extract last segment after the last '.'
    let segment = type_str.rsplit('.').next().unwrap_or(type_str);

    // Convert camelCase to Title Case with spaces
    let mut result = String::new();
    for (i, ch) in segment.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            result.push(' ');
        }
        if i == 0 {
            for upper in ch.to_uppercase() {
                result.push(upper);
            }
        } else {
            result.push(ch);
        }
    }

    // Handle well-known all-caps abbreviations
    result = result
        .replace("Http Request", "HTTP Request")
        .replace("Http", "HTTP")
        .replace("Ftp", "FTP")
        .replace("Ssh", "SSH")
        .replace("Xml", "XML")
        .replace("Csv", "CSV");

    // Handle short names that should be all-caps
    match result.as_str() {
        "If" => "IF".to_string(),
        _ => result,
    }
}

/// Split a node name into (base, trailing_digits) if it ends with optional space + digits.
/// Returns `None` if the name doesn't match the pattern.
fn split_default_name(name: &str) -> Option<&str> {
    // Find the position where trailing digits start (possibly preceded by a space)
    let bytes = name.as_bytes();
    let len = bytes.len();
    if len == 0 {
        return None;
    }

    // Walk backwards over digits
    let digit_end = len;
    let mut pos = len;
    while pos > 0 && bytes[pos - 1].is_ascii_digit() {
        pos -= 1;
    }
    if pos == digit_end {
        // No trailing digits
        return None;
    }

    // Optionally skip a single space before the digits
    let base_end = if pos > 0 && bytes[pos - 1] == b' ' {
        pos - 1
    } else {
        pos
    };

    if base_end == 0 {
        return None;
    }

    Some(&name[..base_end])
}

fn check_no_default_names(workflow: &Value, severity: LintSeverity) -> Vec<LintDiagnostic> {
    let mut out = Vec::new();

    for node in nodes(workflow) {
        let name = node_name(node);
        let Some(type_str) = node_type(node) else {
            continue;
        };
        let display = type_to_display_name(type_str);

        if let Some(base) = split_default_name(&name)
            && base == display
        {
            out.push(LintDiagnostic {
                severity,
                rule: "no-default-names".to_string(),
                node: Some(name.clone()),
                message: "Node uses a default name — consider renaming to describe its purpose."
                    .to_string(),
            });
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Rule: no-empty-expressions
// ---------------------------------------------------------------------------

/// Check if a string is an empty n8n expression: `={{ }}` with optional whitespace inside.
fn is_empty_expression(s: &str) -> bool {
    let Some(inner) = s.strip_prefix("={{") else {
        return false;
    };
    let Some(inner) = inner.strip_suffix("}}") else {
        return false;
    };
    inner.trim().is_empty()
}

fn check_no_empty_expressions(workflow: &Value, severity: LintSeverity) -> Vec<LintDiagnostic> {
    let mut out = Vec::new();

    for node in nodes(workflow) {
        let name = node_name(node);
        if let Some(params) = node.get("parameters") {
            walk_for_empty_expressions(params, severity, &name, &mut out);
        }
    }

    out
}

fn walk_for_empty_expressions(
    value: &Value,
    severity: LintSeverity,
    node_name: &str,
    out: &mut Vec<LintDiagnostic>,
) {
    match value {
        Value::String(s) if is_empty_expression(s) => {
            out.push(LintDiagnostic {
                severity,
                rule: "no-empty-expressions".to_string(),
                node: Some(node_name.to_string()),
                message: "Empty expression `={{ }}` found.".to_string(),
            });
        }
        Value::Array(arr) => {
            for item in arr {
                walk_for_empty_expressions(item, severity, node_name, out);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                walk_for_empty_expressions(item, severity, node_name, out);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::collections::BTreeMap;

    use super::*;

    fn empty_config() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    #[test]
    fn no_hardcoded_urls_flags_literal_url() {
        let wf = json!({
            "nodes": [{
                "name": "HTTP Request",
                "type": "n8n-nodes-base.httpRequest",
                "parameters": { "url": "https://example.com/api" }
            }]
        });
        let diags = lint_workflow(&wf, &empty_config(), Some("no-hardcoded-urls"));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].rule, "no-hardcoded-urls");
        assert_eq!(diags[0].node.as_deref(), Some("HTTP Request"));
    }

    #[test]
    fn no_hardcoded_urls_ignores_expression() {
        let wf = json!({
            "nodes": [{
                "name": "HTTP Request",
                "type": "n8n-nodes-base.httpRequest",
                "parameters": { "url": "={{ $env.API_URL }}" }
            }]
        });
        let diags = lint_workflow(&wf, &empty_config(), Some("no-hardcoded-urls"));
        assert!(diags.is_empty());
    }

    #[test]
    fn no_hardcoded_urls_checks_options_url() {
        let wf = json!({
            "nodes": [{
                "name": "HTTP Request",
                "type": "n8n-nodes-base.httpRequest",
                "parameters": {
                    "url": "={{ $env.API_URL }}",
                    "options": { "url": "https://fallback.com" }
                }
            }]
        });
        let diags = lint_workflow(&wf, &empty_config(), Some("no-hardcoded-urls"));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("options"));
    }

    #[test]
    fn no_disabled_nodes_flags_disabled() {
        let wf = json!({
            "nodes": [
                { "name": "Active", "type": "n8n-nodes-base.set" },
                { "name": "Paused", "type": "n8n-nodes-base.set", "disabled": true }
            ]
        });
        let diags = lint_workflow(&wf, &empty_config(), Some("no-disabled-nodes"));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].node.as_deref(), Some("Paused"));
    }

    #[test]
    fn no_disabled_nodes_clean() {
        let wf = json!({
            "nodes": [
                { "name": "Active", "type": "n8n-nodes-base.set" }
            ]
        });
        let diags = lint_workflow(&wf, &empty_config(), Some("no-disabled-nodes"));
        assert!(diags.is_empty());
    }

    #[test]
    fn require_error_handler_flags_missing() {
        let mut config = BTreeMap::new();
        config.insert("require-error-handler".to_string(), "warn".to_string());
        let wf = json!({ "nodes": [{ "name": "Start", "type": "n8n-nodes-base.start" }] });
        let diags = lint_workflow(&wf, &config, Some("require-error-handler"));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].node.is_none());
    }

    #[test]
    fn require_error_handler_passes_when_present() {
        let mut config = BTreeMap::new();
        config.insert("require-error-handler".to_string(), "warn".to_string());
        let wf = json!({
            "nodes": [{ "name": "Error", "type": "n8n-nodes-base.errorTrigger" }]
        });
        let diags = lint_workflow(&wf, &config, Some("require-error-handler"));
        assert!(diags.is_empty());
    }

    #[test]
    fn no_default_names_flags_numbered() {
        let wf = json!({
            "nodes": [{ "name": "Set1", "type": "n8n-nodes-base.set" }]
        });
        let diags = lint_workflow(&wf, &empty_config(), Some("no-default-names"));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].node.as_deref(), Some("Set1"));
    }

    #[test]
    fn no_default_names_with_space() {
        let wf = json!({
            "nodes": [{ "name": "IF 2", "type": "n8n-nodes-base.if" }]
        });
        let diags = lint_workflow(&wf, &empty_config(), Some("no-default-names"));
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn no_default_names_clean() {
        let wf = json!({
            "nodes": [{ "name": "Check Order Status", "type": "n8n-nodes-base.if" }]
        });
        let diags = lint_workflow(&wf, &empty_config(), Some("no-default-names"));
        assert!(diags.is_empty());
    }

    #[test]
    fn no_empty_expressions_flags_empty() {
        let wf = json!({
            "nodes": [{
                "name": "Set",
                "type": "n8n-nodes-base.set",
                "parameters": { "value": "={{  }}" }
            }]
        });
        let diags = lint_workflow(&wf, &empty_config(), Some("no-empty-expressions"));
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn no_empty_expressions_ignores_non_empty() {
        let wf = json!({
            "nodes": [{
                "name": "Set",
                "type": "n8n-nodes-base.set",
                "parameters": { "value": "={{ $json.name }}" }
            }]
        });
        let diags = lint_workflow(&wf, &empty_config(), Some("no-empty-expressions"));
        assert!(diags.is_empty());
    }

    #[test]
    fn config_override_turns_rule_off() {
        let wf = json!({
            "nodes": [{ "name": "Paused", "type": "n8n-nodes-base.set", "disabled": true }]
        });
        let mut config = BTreeMap::new();
        config.insert("no-disabled-nodes".to_string(), "off".to_string());
        let diags = lint_workflow(&wf, &config, None);
        // no-disabled-nodes is off, so no diagnostic for it
        assert!(diags.iter().all(|d| d.rule != "no-disabled-nodes"));
    }

    #[test]
    fn only_rule_filter() {
        let wf = json!({
            "nodes": [{
                "name": "HTTP Request",
                "type": "n8n-nodes-base.httpRequest",
                "parameters": { "url": "https://example.com" },
                "disabled": true
            }]
        });
        let diags = lint_workflow(&wf, &empty_config(), Some("no-disabled-nodes"));
        assert!(diags.iter().all(|d| d.rule == "no-disabled-nodes"));
        assert_eq!(diags.len(), 1);
    }
}
