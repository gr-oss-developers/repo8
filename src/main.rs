use std::net::SocketAddr;

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

const DEFAULT_PORT: u16 = 8080;

#[derive(Clone)]
struct AppState {
    http: reqwest::Client,
    github_token: Option<String>,
}

#[derive(Deserialize)]
struct ReposQuery {
    username: String,
}

/// The repo fields exposed by the API; serde drops every other field of
/// GitHub's payload.
#[derive(Debug, Deserialize, Serialize)]
struct RepoSummary {
    name: String,
    full_name: String,
    html_url: String,
    description: Option<String>,
    language: Option<String>,
    stargazers_count: u64,
    fork: bool,
}

#[tokio::main]
async fn main() {
    let port = match std::env::var("PORT") {
        Ok(value) => match value.parse() {
            Ok(port) => port,
            Err(_) => {
                eprintln!("warning: PORT={value} is not a valid port, using {DEFAULT_PORT}");
                DEFAULT_PORT
            }
        },
        Err(_) => DEFAULT_PORT,
    };
    let state = AppState {
        http: reqwest::Client::builder()
            .user_agent("github-repos-api")
            .build()
            .expect("failed to build HTTP client"),
        github_token: std::env::var("GITHUB_TOKEN")
            .ok()
            .filter(|token| !token.is_empty()),
    };
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind listen address");
    println!("listening on {addr}");
    axum::serve(listener, router(state))
        .await
        .expect("server error");
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/repos", get(list_repos))
        .route("/health", get(health))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

async fn list_repos(
    State(state): State<AppState>,
    query: Result<Query<ReposQuery>, QueryRejection>,
) -> Response {
    let Query(ReposQuery { username }) = match query {
        Ok(query) => query,
        Err(_) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "missing required query parameter: username",
            )
        }
    };
    let username = username.trim();
    if username.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "missing required query parameter: username",
        );
    }

    let mut url =
        reqwest::Url::parse("https://api.github.com/users").expect("hardcoded base URL is valid");
    url.path_segments_mut()
        .expect("hardcoded base URL accepts path segments")
        .push(username)
        .push("repos");
    // A single page of 100; larger accounts are truncated (documented in the
    // README).
    url.query_pairs_mut().append_pair("per_page", "100");

    let mut request = state.http.get(url);
    if let Some(token) = &state.github_token {
        request = request.bearer_auth(token);
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(_) => return json_error(StatusCode::BAD_GATEWAY, "failed to reach the GitHub API"),
    };

    let status = response.status();
    if !status.is_success() {
        let remaining = response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|value| value.to_str().ok());
        let (status, message) = upstream_error(status, remaining);
        return json_error(status, message);
    }
    let repos: Vec<RepoSummary> = match response.json().await {
        Ok(repos) => repos,
        Err(_) => {
            return json_error(
                StatusCode::BAD_GATEWAY,
                "failed to parse the GitHub API response",
            )
        }
    };
    Json(repos).into_response()
}

/// Maps a GitHub API error to our status and message. GitHub signals rate
/// limiting with 429, or with 403 plus `X-RateLimit-Remaining: 0`.
fn upstream_error(
    status: StatusCode,
    rate_limit_remaining: Option<&str>,
) -> (StatusCode, &'static str) {
    if status == StatusCode::NOT_FOUND {
        return (StatusCode::NOT_FOUND, "GitHub user not found");
    }
    let rate_limited = status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::FORBIDDEN && rate_limit_remaining == Some("0"));
    if rate_limited {
        return (
            StatusCode::BAD_GATEWAY,
            "GitHub API rate limit exceeded; set GITHUB_TOKEN to raise the limit",
        );
    }
    (StatusCode::BAD_GATEWAY, "GitHub API request failed")
}

fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": message.into() }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    fn test_state() -> AppState {
        AppState {
            http: reqwest::Client::new(),
            github_token: None,
        }
    }

    #[test]
    fn deserializes_github_payload_to_concise_summaries() {
        let payload = r#"[
            {
                "id": 7,
                "node_id": "R_1",
                "name": "repo8",
                "full_name": "pavlovic-ivan/repo8",
                "html_url": "https://github.com/pavlovic-ivan/repo8",
                "description": "Repository 8",
                "language": "Rust",
                "stargazers_count": 3,
                "fork": false,
                "private": false,
                "forks_count": 0
            },
            {
                "id": 8,
                "node_id": "R_2",
                "name": "forked",
                "full_name": "pavlovic-ivan/forked",
                "html_url": "https://github.com/pavlovic-ivan/forked",
                "description": null,
                "language": null,
                "stargazers_count": 0,
                "fork": true
            }
        ]"#;

        let repos: Vec<RepoSummary> = serde_json::from_str(payload).unwrap();
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].name, "repo8");
        assert_eq!(repos[0].stargazers_count, 3);
        assert!(!repos[0].fork);
        assert_eq!(repos[1].description, None);
        assert_eq!(repos[1].language, None);
        assert!(repos[1].fork);

        let summary = serde_json::to_value(&repos).unwrap()[0]
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(summary.len(), 7);
        for key in [
            "name",
            "full_name",
            "html_url",
            "description",
            "language",
            "stargazers_count",
            "fork",
        ] {
            assert!(summary.contains_key(key), "missing key {key}");
        }
        assert!(!summary.contains_key("private"));
        assert!(!summary.contains_key("forks_count"));
    }

    #[test]
    fn maps_upstream_errors() {
        let (status, _) = upstream_error(StatusCode::NOT_FOUND, Some("30"));
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, message) = upstream_error(StatusCode::FORBIDDEN, Some("0"));
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(message.contains("rate limit"));

        let (status, _) = upstream_error(StatusCode::FORBIDDEN, Some("12"));
        assert_eq!(status, StatusCode::BAD_GATEWAY);

        let (status, message) = upstream_error(StatusCode::TOO_MANY_REQUESTS, None);
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(message.contains("rate limit"));

        let (status, _) = upstream_error(StatusCode::INTERNAL_SERVER_ERROR, Some("9"));
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn repos_without_username_returns_json_400() {
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/repos")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body,
            json!({"error": "missing required query parameter: username"})
        );
    }

    #[tokio::test]
    async fn repos_with_blank_username_returns_json_400() {
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/repos?username=")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body,
            json!({"error": "missing required query parameter: username"})
        );
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
