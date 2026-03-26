use std::{collections::BTreeMap, time::Duration};

use reqwest::{Method, StatusCode, Url};
use serde::Serialize;
use serde_json::Value;

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

    pub async fn get_workflow_by_id(&self, workflow_id: &str) -> Result<Option<Value>, AppError> {
        self.request_json_optional(Method::GET, &format!("workflows/{workflow_id}"), &[], None)
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

        let mut request = self.client.request(method, url);
        for (key, value) in headers {
            request = request.header(key, value);
        }
        if let Some(body) = body {
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
            return Err(AppError::api(
                self.command,
                format!("trigger.http_{}", status.as_u16()),
                format!("Webhook returned HTTP {}.", status.as_u16()),
            ));
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
mod tests {
    use serde_json::json;

    use super::{ListOptions, append_matching_workflows};

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
}
