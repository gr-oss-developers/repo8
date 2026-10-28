# repo8
Repository 8 created with Terraform

## github-repos-api

A small REST service that returns the public repositories of a GitHub user.

### Endpoints

- `GET /repos?username=<user>` — a JSON array of the user's public repos, each with `name`, `full_name`, `html_url`, `description`, `language`, `stargazers_count` and `fork`. Follows GitHub pagination up to 10 pages, so at most 1000 repos are returned.
- `GET /health` — returns `200` for readiness polling.

Errors are JSON objects with an `error` field: `400` when the `username` query parameter is missing, blank or malformed, `404` when the user does not exist, `502` when GitHub rate-limits the request or the upstream call otherwise fails.

### Configuration

- `PORT` — listen port (default `8080`; the service binds on `0.0.0.0`).
- `GITHUB_TOKEN` — optional; sent to GitHub as `Authorization: Bearer <token>` to raise the rate limit.

### Usage

```bash
cargo run
curl 'http://localhost:8080/repos?username=pavlovic-ivan'
```
