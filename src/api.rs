use std::{collections::BTreeMap, time::Duration};

use chrono::{DateTime, Utc};
use reqwest::{Method, StatusCode, Url};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{config::InstanceConfig, error::AppError};

#[derive(Debug, Clone)]
pub struct ApiClient {
    command: &'static str,
    client: reqwest::Client,
    base_url: Url,
    api_base: Url,
    token: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriggerResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

#[derive(Debug, Clone)]
pub struct ListOptions {
    pub limit: u16,
    pub active: Option<bool>,
    pub name_filter: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExecutionListOptions {
    pub limit: u16,
    pub workflow_id: Option<String>,
    pub status: Option<String>,
    pub since: Option<DateTime<Utc>>,
}

impl ApiClient {
    pub fn new(
        command: &'static str,
        instance: &InstanceConfig,
        token: String,
    ) -> Result<Self, AppError> {
        let base_url = Url::parse(instance.base_url.trim_end_matches('/')).map_err(|err| {
            AppError::config(
                command,
                format!("Invalid base URL `{}`: {err}", instance.base_url),
            )
        })?;
        let mut api_base = base_url.clone();
        let version = instance.api_version.trim_start_matches('v');
        api_base
            .path_segments_mut()
            .map_err(|_| {
                AppError::config(
                    command,
                    "The configured base URL cannot be used as an API root.",
                )
            })?
            .extend(["api", &format!("v{version}")]);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|err| {
                AppError::network(command, format!("Failed to build HTTP client: {err}"))
            })?;

        Ok(Self {
            command,
            client,
            base_url,
            api_base,
            token,
        })
    }

    pub async fn list_workflows(&self, options: &ListOptions) -> Result<Vec<Value>, AppError> {
        let mut query = vec![("limit".to_string(), options.limit.to_string())];
        if let Some(active) = options.active {
            query.push(("active".to_string(), active.to_string()));
        }

        let mut next_cursor: Option<String> = None;
        let mut results = Vec::new();

        loop {
            let mut page_query = query.clone();
            if let Some(cursor) = &next_cursor {
                page_query.push(("cursor".to_string(), cursor.clone()));
            }

            let page = self
                .request_json(Method::GET, "workflows", &page_query, None)
                .await?;

            let page_data = page
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .or_else(|| page.as_array().cloned())
                .ok_or_else(|| {
                    AppError::api(
                        self.command,
                        "api.invalid_response",
                        "Expected a paginated workflow list response.",
                    )
                })?;

            if append_matching_workflows(&mut results, &page_data, options) {
                break;
            }

            next_cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            if next_cursor.is_none() {
                break;
            }
        }

        Ok(results)
    }

    pub async fn list_executions(
        &self,
        options: &ExecutionListOptions,
    ) -> Result<Vec<Value>, AppError> {
        if options.limit == 0 {
            return Ok(Vec::new());
        }

        let mut query = vec![("limit".to_string(), options.limit.to_string())];
        if let Some(workflow_id) = &options.workflow_id {
            query.push(("workflowId".to_string(), workflow_id.clone()));
        }
        if let Some(status) = &options.status {
            query.push(("status".to_string(), status.clone()));
        }

        let mut next_cursor: Option<String> = None;
        let mut results = Vec::new();

        loop {
            let mut page_query = query.clone();
            if let Some(cursor) = &next_cursor {
                page_query.push(("cursor".to_string(), cursor.clone()));
            }

            let page = self
                .request_json(Method::GET, "executions", &page_query, None)
                .await?;

            let page_data = page
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .or_else(|| page.as_array().cloned())
                .ok_or_else(|| {
                    AppError::api(
                        self.command,
                        "api.invalid_response",
                        "Expected a paginated execution list response.",
                    )
                })?;
            let page_crossed_since_cutoff = options
                .since
                .as_ref()
                .is_some_and(|since| page_crosses_since_cutoff(&page_data, since));

            if append_matching_executions(&mut results, &page_data, options) {
                break;
            }
            if page_crossed_since_cutoff {
                break;
            }

            next_cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            if next_cursor.is_none() {
                break;
            }
        }

        Ok(results)
    }

    pub async fn get_workflow_by_id(&self, workflow_id: &str) -> Result<Option<Value>, AppError> {
        self.request_json_optional(Method::GET, &format!("workflows/{workflow_id}"), &[], None)
            .await
    }

    pub async fn get_execution(
        &self,
        execution_id: &str,
        include_data: bool,
    ) -> Result<Option<Value>, AppError> {
        let mut query = Vec::new();
        if include_data {
            query.push(("includeData".to_string(), "true".to_string()));
        }
        self.request_json_optional(
            Method::GET,
            &format!("executions/{execution_id}"),
            &query,
            None,
        )
        .await
    }

    pub async fn resolve_workflow(&self, identifier: &str) -> Result<Value, AppError> {
        if let Some(workflow) = self.get_workflow_by_id(identifier).await? {
            return Ok(extract_data(workflow));
        }

        let candidates = self
            .list_workflows(&ListOptions {
                limit: 250,
                active: None,
                name_filter: Some(identifier.to_string()),
            })
            .await?;

        let exact: Vec<Value> = candidates
            .into_iter()
            .filter(|workflow| workflow.get("name").and_then(Value::as_str) == Some(identifier))
            .collect();

        match exact.len() {
            0 => Err(AppError::not_found(
                self.command,
                format!("No workflow matched `{identifier}`."),
            )),
            1 => {
                let workflow_id = exact[0].get("id").and_then(Value::as_str).ok_or_else(|| {
                    AppError::api(
                        self.command,
                        "api.invalid_response",
                        "Workflow list item was missing `id`.",
                    )
                })?;
                self.get_workflow_by_id(workflow_id)
                    .await?
                    .map(extract_data)
                    .ok_or_else(|| {
                        AppError::not_found(
                            self.command,
                            format!(
                                "Workflow `{identifier}` disappeared before it could be fetched."
                            ),
                        )
                    })
            }
            _ => Err(AppError::api(
                self.command,
                "workflow.ambiguous",
                format!("Multiple workflows matched `{identifier}`."),
            )
            .with_suggestion("Use a workflow ID instead of the display name.")),
        }
    }

    pub async fn update_workflow(
        &self,
        workflow_id: &str,
        payload: &Value,
    ) -> Result<Value, AppError> {
        let response = self
            .request_json(
                Method::PUT,
                &format!("workflows/{workflow_id}"),
                &[],
                Some(payload),
            )
            .await?;
        Ok(extract_data(response))
    }

    pub async fn create_workflow(&self, payload: &Value) -> Result<Value, AppError> {
        let response = self
            .request_json(Method::POST, "workflows", &[], Some(payload))
            .await?;
        Ok(extract_data(response))
    }

    pub async fn activate_workflow(&self, workflow_id: &str) -> Result<(), AppError> {
        self.request_json(
            Method::POST,
            &format!("workflows/{workflow_id}/activate"),
            &[],
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn deactivate_workflow(&self, workflow_id: &str) -> Result<(), AppError> {
        self.request_json(
            Method::POST,
            &format!("workflows/{workflow_id}/deactivate"),
            &[],
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn archive_workflow(
        &self,
        workflow_id: &str,
        session_cookie: &str,
        browser_id: &str,
    ) -> Result<(), AppError> {
        let path = format!("workflows/{workflow_id}/archive");
        self.request_rest_json_optional(
            Method::POST,
            &path,
            &[],
            None,
            session_cookie,
            browser_id,
        )
        .await?;
        Ok(())
    }

    pub async fn unarchive_workflow(
        &self,
        workflow_id: &str,
        session_cookie: &str,
        browser_id: &str,
    ) -> Result<(), AppError> {
        let path = format!("workflows/{workflow_id}/unarchive");
        self.request_rest_json_optional(
            Method::POST,
            &path,
            &[],
            None,
            session_cookie,
            browser_id,
        )
        .await?;
        Ok(())
    }

    pub async fn delete_workflow(&self, workflow_id: &str) -> Result<(), AppError> {
        self.request_json_optional(
            Method::DELETE,
            &format!("workflows/{workflow_id}"),
            &[],
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn get_credential_schema(&self, credential_type: &str) -> Result<Value, AppError> {
        self.request_json(
            Method::GET,
            &format!("credentials/schema/{credential_type}"),
            &[],
            None,
        )
        .await
    }

    pub async fn list_credentials_public(&self) -> Result<Vec<Value>, AppError> {
        let mut next_cursor: Option<String> = None;
        let mut results = Vec::new();

        loop {
            let mut query = vec![("limit".to_string(), "250".to_string())];
            if let Some(cursor) = &next_cursor {
                query.push(("cursor".to_string(), cursor.clone()));
            }

            let page = self
                .request_json_optional(Method::GET, "credentials", &query, None)
                .await?
                .ok_or_else(|| {
                    AppError::api(
                        self.command,
                        "api.http_404",
                        "The public credential inventory endpoint is not available.",
                    )
                })?;
            let page_data = page
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .or_else(|| page.as_array().cloned())
                .ok_or_else(|| {
                    AppError::api(
                        self.command,
                        "api.invalid_response",
                        "Expected a paginated credential list response.",
                    )
                })?;

            results.extend(page_data);
            next_cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            if next_cursor.is_none() {
                break;
            }
        }

        Ok(results)
    }

    pub async fn probe_credentials_public(&self) -> Result<(), AppError> {
        self.request_json_optional(
            Method::GET,
            "credentials",
            &[("limit".to_string(), "1".to_string())],
            None,
        )
        .await?
        .ok_or_else(|| {
            AppError::api(
                self.command,
                "api.http_404",
                "The public credential inventory endpoint is not available.",
            )
        })
        .map(|_| ())
    }

    pub async fn list_credentials_rest_session(
        &self,
        session_cookie: &str,
        browser_id: &str,
    ) -> Result<Vec<Value>, AppError> {
        let response = self
            .request_rest_json_optional(
                Method::GET,
                "credentials",
                &[("includeData".to_string(), "false".to_string())],
                None,
                session_cookie,
                browser_id,
            )
            .await?
            .ok_or_else(|| {
                AppError::api(
                    self.command,
                    "api.http_404",
                    "The internal REST credential inventory endpoint is not available.",
                )
            })?;

        response
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| response.as_array().cloned())
            .ok_or_else(|| {
                AppError::api(
                    self.command,
                    "api.invalid_response",
                    "Expected an internal REST credential list response.",
                )
            })
    }

    pub async fn probe_credentials_rest_session(
        &self,
        session_cookie: &str,
        browser_id: &str,
    ) -> Result<(), AppError> {
        self.request_rest_json_optional(
            Method::GET,
            "credentials",
            &[("includeData".to_string(), "false".to_string())],
            None,
            session_cookie,
            browser_id,
        )
        .await?
        .ok_or_else(|| {
            AppError::api(
                self.command,
                "api.http_404",
                "The internal REST credential inventory endpoint is not available.",
            )
        })
        .map(|_| ())
    }

    pub async fn trigger(
        &self,
        target: &str,
        method: &str,
        headers: &[(String, String)],
        query: &[(String, String)],
        body: Option<Vec<u8>>,
    ) -> Result<TriggerResponse, AppError> {
        let method = Method::from_bytes(method.as_bytes()).map_err(|err| {
            AppError::usage(
                self.command,
                format!("Invalid HTTP method `{method}`: {err}"),
            )
        })?;

        let mut url = if target.starts_with("http://") || target.starts_with("https://") {
            Url::parse(target).map_err(|err| {
                AppError::usage(
                    self.command,
                    format!("Invalid trigger URL `{target}`: {err}"),
                )
            })?
        } else {
            self.base_url
                .join(target.trim_start_matches('/'))
                .map_err(|err| {
                    AppError::usage(
                        self.command,
                        format!(
                            "Failed to resolve trigger target `{target}` against {}: {err}",
                            self.base_url
                        ),
                    )
                })?
        };

        for (key, value) in query {
            url.query_pairs_mut().append_pair(key, value);
        }

        let request_path = url.path().to_string();
        let has_content_type = headers
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case("content-type"));
        let mut request = self.client.request(method, url);
        for (key, value) in headers {
            request = request.header(key, value);
        }
        if let Some(body) = body {
            if !has_content_type && body_looks_like_json(&body) {
                request = request.header("Content-Type", "application/json");
            }
            request = request.body(body);
        }

        let response = request.send().await.map_err(|err| {
            AppError::network(self.command, format!("Webhook request failed: {err}"))
        })?;

        let status = response.status();
        let headers = response
            .headers()
            .iter()
            .map(|(key, value)| {
                (
                    key.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect::<BTreeMap<_, _>>();

        let bytes = response.bytes().await.map_err(|err| {
            AppError::network(
                self.command,
                format!("Failed to read webhook response: {err}"),
            )
        })?;
        let body = parse_body(bytes.as_ref());

        if !status.is_success() {
            let mut error = AppError::api(
                self.command,
                format!("trigger.http_{}", status.as_u16()),
                format_trigger_http_error(status, &request_path, &body),
            )
            .with_json_data(json!({
                "status": status.as_u16(),
                "headers": headers,
                "body": body,
            }));
            if let Some(suggestion) = trigger_error_suggestion(status, &request_path) {
                error = error.with_suggestion(suggestion);
            }
            return Err(error);
        }

        Ok(TriggerResponse {
            status: status.as_u16(),
            headers,
            body,
        })
    }

    async fn request_json(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Value, AppError> {
        self.request_json_optional(method, path, query, body)
            .await?
            .ok_or_else(|| {
                AppError::api(
                    self.command,
                    "api.empty_response",
                    "The API returned an empty response body.",
                )
            })
    }

    async fn request_json_optional(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Option<Value>, AppError> {
        let mut url = self.api_base.clone();
        url.path_segments_mut()
            .map_err(|_| {
                AppError::config(
                    self.command,
                    "The configured API base URL cannot be extended.",
                )
            })?
            .extend(path.trim_start_matches('/').split('/'));

        let mut request = self
            .client
            .request(method, url)
            .header("Accept", "application/json")
            .header("X-N8N-API-KEY", &self.token)
            .query(query);
        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await.map_err(|err| {
            AppError::network(self.command, format!("Request to n8n failed: {err}"))
        })?;
        let status = response.status();
        let body = response.text().await.map_err(|err| {
            AppError::network(self.command, format!("Failed to read n8n response: {err}"))
        })?;

        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !status.is_success() {
            let message = parse_error_message(&body)
                .unwrap_or_else(|| format!("n8n returned HTTP {}.", status.as_u16()));
            return Err(AppError::api(
                self.command,
                format!("api.http_{}", status.as_u16()),
                message,
            ));
        }

        if body.trim().is_empty() {
            return Ok(None);
        }

        let parsed: Value = serde_json::from_str(&body).map_err(|err| {
            AppError::api(
                self.command,
                "api.invalid_json",
                format!("n8n returned invalid JSON: {err}"),
            )
        })?;
        Ok(Some(parsed))
    }

    async fn request_rest_json_optional(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&Value>,
        session_cookie: &str,
        browser_id: &str,
    ) -> Result<Option<Value>, AppError> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|_| {
                AppError::config(
                    self.command,
                    "The configured base URL cannot be used as an internal REST root.",
                )
            })?
            .extend(["rest"])
            .extend(path.trim_start_matches('/').split('/'));

        let mut request = self
            .client
            .request(method, url)
            .header("Accept", "application/json")
            .header("Cookie", session_cookie)
            .header("browser-id", browser_id)
            .query(query);
        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await.map_err(|err| {
            AppError::network(
                self.command,
                format!("Request to n8n internal REST API failed: {err}"),
            )
        })?;
        let status = response.status();
        let body = response.text().await.map_err(|err| {
            AppError::network(
                self.command,
                format!("Failed to read n8n internal REST response: {err}"),
            )
        })?;

        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(AppError::auth(
                self.command,
                "Session credentials expired or invalid. Re-run `n8nc auth session add` to update.",
            ));
        }

        if !status.is_success() {
            let message = parse_error_message(&body).unwrap_or_else(|| {
                format!("n8n internal REST API returned HTTP {}.", status.as_u16())
            });
            return Err(AppError::api(
                self.command,
                format!("api.http_{}", status.as_u16()),
                message,
            ));
        }

        if body.trim().is_empty() {
            return Ok(None);
        }

        let parsed: Value = serde_json::from_str(&body).map_err(|err| {
            AppError::api(
                self.command,
                "api.invalid_json",
                format!("n8n internal REST API returned invalid JSON: {err}"),
            )
        })?;
        Ok(Some(parsed))
    }
}

fn extract_data(value: Value) -> Value {
    value.get("data").cloned().unwrap_or(value)
}

fn parse_error_message(body: &str) -> Option<String> {
    let json: Value = serde_json::from_str(body).ok()?;
    json.get("message")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            json.get("error")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn parse_body(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(bytes).to_string()))
}

fn body_looks_like_json(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bytes).is_ok()
}

fn format_trigger_http_error(status: StatusCode, path: &str, body: &Value) -> String {
    let mut message = format!("Webhook returned HTTP {} for `{path}`.", status.as_u16());
    let body_summary = summarize_trigger_body(body);
    if !body_summary.is_empty() {
        message.push_str(" Response body: ");
        message.push_str(&body_summary);
    }
    message
}

fn summarize_trigger_body(body: &Value) -> String {
    match body {
        Value::Null => String::new(),
        Value::String(text) => text.trim().chars().take(200).collect(),
        Value::Object(object) => object
            .get("message")
            .and_then(Value::as_str)
            .map(|text| text.trim().chars().take(200).collect())
            .unwrap_or_else(|| body.to_string().chars().take(200).collect()),
        _ => body.to_string().chars().take(200).collect(),
    }
}

fn trigger_error_suggestion(status: StatusCode, path: &str) -> Option<&'static str> {
    if status != StatusCode::NOT_FOUND {
        return None;
    }
    if path.starts_with("/webhook-test/") {
        return Some(
            "Test webhook URLs only work while the workflow is listening in test mode in n8n.",
        );
    }
    if path.starts_with("/webhook/") {
        return Some(
            "Production webhook 404s usually mean the path is wrong, the workflow is inactive, or n8n has not registered the webhook yet.",
        );
    }
    None
}

fn append_matching_workflows(
    results: &mut Vec<Value>,
    page_data: &[Value],
    options: &ListOptions,
) -> bool {
    let limit = usize::from(options.limit);
    for workflow in page_data {
        if let Some(filter) = &options.name_filter {
            let name = workflow
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !name
                .to_ascii_lowercase()
                .contains(&filter.to_ascii_lowercase())
            {
                continue;
            }
        }
        results.push(workflow.clone());
        if results.len() >= limit {
            return true;
        }
    }
    false
}

#[cfg(test)]
fn append_capped_values(results: &mut Vec<Value>, page_data: &[Value], limit: usize) -> bool {
    for value in page_data {
        results.push(value.clone());
        if results.len() >= limit {
            return true;
        }
    }
    false
}

fn append_matching_executions(
    results: &mut Vec<Value>,
    page_data: &[Value],
    options: &ExecutionListOptions,
) -> bool {
    let limit = usize::from(options.limit);
    for execution in page_data {
        if !execution_matches_time_filter(execution, options.since.as_ref()) {
            continue;
        }
        results.push(execution.clone());
        if results.len() >= limit {
            return true;
        }
    }
    false
}

fn execution_matches_time_filter(execution: &Value, since: Option<&DateTime<Utc>>) -> bool {
    let Some(since) = since else {
        return true;
    };
    let Some(timestamp) = execution_filter_timestamp(execution) else {
        return false;
    };
    timestamp >= *since
}

fn execution_filter_timestamp(execution: &Value) -> Option<DateTime<Utc>> {
    for key in ["startedAt", "waitTill", "stoppedAt"] {
        let Some(raw) = execution.get(key).and_then(Value::as_str) else {
            continue;
        };
        if let Ok(timestamp) = DateTime::parse_from_rfc3339(raw) {
            return Some(timestamp.with_timezone(&Utc));
        }
    }
    None
}

fn page_crosses_since_cutoff(page_data: &[Value], since: &DateTime<Utc>) -> bool {
    page_data
        .iter()
        .rev()
        .find_map(execution_filter_timestamp)
        .is_some_and(|oldest| oldest < *since)
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use serde_json::json;
    use wiremock::{
        Match, Mock, MockServer, Request, ResponseTemplate,
        matchers::{header, method, path, query_param},
    };

    use crate::config::InstanceConfig;

    use super::{
        ApiClient, ExecutionListOptions, ListOptions, append_capped_values,
        append_matching_executions, append_matching_workflows, execution_filter_timestamp,
        page_crosses_since_cutoff,
    };

    #[derive(Debug)]
    struct MissingQueryParam(&'static str);

    impl Match for MissingQueryParam {
        fn matches(&self, request: &Request) -> bool {
            !request
                .url
                .query_pairs()
                .any(|(key, _)| key.as_ref() == self.0)
        }
    }

    #[test]
    fn append_matching_workflows_honors_limit_and_filter() {
        let page = vec![
            json!({"id":"a","name":"Alpha"}),
            json!({"id":"b","name":"Beta"}),
            json!({"id":"c","name":"Alphabet"}),
        ];

        let mut limited = Vec::new();
        let reached_limit = append_matching_workflows(
            &mut limited,
            &page,
            &ListOptions {
                limit: 2,
                active: None,
                name_filter: None,
            },
        );
        assert!(reached_limit);
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0]["id"], "a");
        assert_eq!(limited[1]["id"], "b");

        let mut filtered = Vec::new();
        let reached_limit = append_matching_workflows(
            &mut filtered,
            &page,
            &ListOptions {
                limit: 5,
                active: None,
                name_filter: Some("alpha".to_string()),
            },
        );
        assert!(!reached_limit);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0]["id"], "a");
        assert_eq!(filtered[1]["id"], "c");
    }

    #[test]
    fn append_capped_values_stops_at_limit() {
        let page = vec![json!({"id":"a"}), json!({"id":"b"}), json!({"id":"c"})];
        let mut values = Vec::new();

        let reached_limit = append_capped_values(&mut values, &page, 2);

        assert!(reached_limit);
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["id"], "a");
        assert_eq!(values[1]["id"], "b");
    }

    #[test]
    fn append_matching_executions_honors_since_filter() {
        let page = vec![
            json!({"id":"a","startedAt":"2026-03-26T12:00:00Z"}),
            json!({"id":"b","waitTill":"2026-03-26T12:05:00Z"}),
            json!({"id":"c","startedAt":"2026-03-26T11:59:59Z"}),
            json!({"id":"d"}),
        ];
        let since = DateTime::parse_from_rfc3339("2026-03-26T12:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        let mut executions = Vec::new();

        let reached_limit = append_matching_executions(
            &mut executions,
            &page,
            &ExecutionListOptions {
                limit: 2,
                workflow_id: None,
                status: None,
                since: Some(since),
            },
        );

        assert!(reached_limit);
        assert_eq!(executions.len(), 2);
        assert_eq!(executions[0]["id"], "a");
        assert_eq!(executions[1]["id"], "b");
    }

    #[test]
    fn execution_filter_timestamp_prefers_started_then_wait_then_stopped() {
        let execution = json!({
            "waitTill": "2026-03-26T12:05:00Z",
            "startedAt": "2026-03-26T12:00:00Z",
            "stoppedAt": "2026-03-26T12:10:00Z"
        });
        let timestamp = execution_filter_timestamp(&execution).expect("timestamp");

        assert_eq!(timestamp.to_rfc3339(), "2026-03-26T12:00:00+00:00");
    }

    #[test]
    fn page_crosses_since_cutoff_when_oldest_timestamp_is_older() {
        let since = DateTime::parse_from_rfc3339("2026-03-26T12:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        let page = vec![
            json!({"id":"1","startedAt":"2026-03-26T12:05:00Z"}),
            json!({"id":"2","startedAt":"2026-03-26T11:59:59Z"}),
        ];

        assert!(page_crosses_since_cutoff(&page, &since));
    }

    #[tokio::test]
    async fn list_executions_follows_cursor_and_respects_limit() {
        let server = MockServer::start().await;
        let client = test_client(&server);

        Mock::given(method("GET"))
            .and(path("/api/v1/executions"))
            .and(header("x-n8n-api-key", "test-token"))
            .and(query_param("limit", "3"))
            .and(query_param("status", "success"))
            .and(MissingQueryParam("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"id": "1"},
                    {"id": "2"}
                ],
                "nextCursor": "next-1"
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/api/v1/executions"))
            .and(header("x-n8n-api-key", "test-token"))
            .and(query_param("limit", "3"))
            .and(query_param("status", "success"))
            .and(query_param("cursor", "next-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"id": "3"},
                    {"id": "4"}
                ]
            })))
            .mount(&server)
            .await;

        let executions = client
            .list_executions(&ExecutionListOptions {
                limit: 3,
                workflow_id: None,
                status: Some("success".to_string()),
                since: None,
            })
            .await
            .expect("list executions");

        assert_eq!(executions.len(), 3);
        assert_eq!(executions[0]["id"], "1");
        assert_eq!(executions[1]["id"], "2");
        assert_eq!(executions[2]["id"], "3");
    }

    #[tokio::test]
    async fn list_executions_pages_until_since_filter_fills_limit() {
        let server = MockServer::start().await;
        let client = test_client(&server);
        let since = DateTime::parse_from_rfc3339("2026-03-26T12:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);

        Mock::given(method("GET"))
            .and(path("/api/v1/executions"))
            .and(header("x-n8n-api-key", "test-token"))
            .and(query_param("limit", "3"))
            .and(MissingQueryParam("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"id": "1", "startedAt": "2026-03-26T12:02:00Z"},
                    {"id": "2", "startedAt": "2026-03-26T12:01:00Z"}
                ],
                "nextCursor": "next-1"
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/api/v1/executions"))
            .and(header("x-n8n-api-key", "test-token"))
            .and(query_param("limit", "3"))
            .and(query_param("cursor", "next-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"id": "3", "waitTill": "2026-03-26T12:00:30Z"},
                    {"id": "4", "startedAt": "2026-03-26T11:59:00Z"}
                ]
            })))
            .mount(&server)
            .await;

        let executions = client
            .list_executions(&ExecutionListOptions {
                limit: 3,
                workflow_id: None,
                status: None,
                since: Some(since),
            })
            .await
            .expect("list executions");

        assert_eq!(executions.len(), 3);
        assert_eq!(executions[0]["id"], "1");
        assert_eq!(executions[1]["id"], "2");
        assert_eq!(executions[2]["id"], "3");
    }

    #[tokio::test]
    async fn list_executions_stops_after_page_crosses_since_cutoff() {
        let server = MockServer::start().await;
        let client = test_client(&server);
        let since = DateTime::parse_from_rfc3339("2026-03-26T12:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);

        Mock::given(method("GET"))
            .and(path("/api/v1/executions"))
            .and(header("x-n8n-api-key", "test-token"))
            .and(query_param("limit", "3"))
            .and(MissingQueryParam("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {"id": "1", "startedAt": "2026-03-26T12:01:00Z"},
                    {"id": "2", "startedAt": "2026-03-26T11:59:00Z"}
                ],
                "nextCursor": "next-1"
            })))
            .mount(&server)
            .await;

        let executions = client
            .list_executions(&ExecutionListOptions {
                limit: 3,
                workflow_id: None,
                status: None,
                since: Some(since),
            })
            .await
            .expect("list executions");

        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0]["id"], "1");
    }

    #[tokio::test]
    async fn get_execution_includes_details_when_requested() {
        let server = MockServer::start().await;
        let client = test_client(&server);

        Mock::given(method("GET"))
            .and(path("/api/v1/executions/42"))
            .and(header("x-n8n-api-key", "test-token"))
            .and(query_param("includeData", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "42",
                "status": "success",
                "data": {
                    "resultData": {
                        "runData": {}
                    }
                }
            })))
            .mount(&server)
            .await;

        let execution = client
            .get_execution("42", true)
            .await
            .expect("get execution")
            .expect("execution payload");

        assert_eq!(execution["id"], "42");
        assert!(execution.get("data").is_some());
    }

    #[tokio::test]
    async fn create_workflow_posts_payload_and_extracts_data() {
        let server = MockServer::start().await;
        let client = test_client(&server);

        Mock::given(method("POST"))
            .and(path("/api/v1/workflows"))
            .and(header("x-n8n-api-key", "test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "wf-created",
                    "name": "Created Workflow",
                    "nodes": [],
                    "connections": {},
                    "settings": {}
                }
            })))
            .mount(&server)
            .await;

        let workflow = client
            .create_workflow(&json!({
                "name": "Created Workflow",
                "nodes": [],
                "connections": {},
                "settings": {}
            }))
            .await
            .expect("create workflow");

        assert_eq!(workflow["id"], "wf-created");
        assert_eq!(workflow["name"], "Created Workflow");
    }

    #[tokio::test]
    async fn get_execution_returns_none_for_not_found() {
        let server = MockServer::start().await;
        let client = test_client(&server);

        Mock::given(method("GET"))
            .and(path("/api/v1/executions/missing"))
            .and(header("x-n8n-api-key", "test-token"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let execution = client
            .get_execution("missing", false)
            .await
            .expect("get execution");

        assert!(execution.is_none());
    }

    #[tokio::test]
    async fn get_credential_schema_fetches_schema_payload() {
        let server = MockServer::start().await;
        let client = test_client(&server);

        Mock::given(method("GET"))
            .and(path("/api/v1/credentials/schema/httpBasicAuth"))
            .and(header("x-n8n-api-key", "test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "type": "object",
                "properties": {
                    "user": {"type": "string"},
                    "password": {"type": "string"}
                }
            })))
            .mount(&server)
            .await;

        let schema = client
            .get_credential_schema("httpBasicAuth")
            .await
            .expect("credential schema");

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["user"]["type"], "string");
    }

    fn test_client(server: &MockServer) -> ApiClient {
        ApiClient::new("test", &test_instance(server), "test-token".to_string())
            .expect("api client")
    }

    fn test_instance(server: &MockServer) -> InstanceConfig {
        InstanceConfig {
            base_url: server.uri(),
            api_version: "v1".to_string(),
            execute: None,
        }
    }
}
