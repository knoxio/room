# US-BE-025: Logging

**As a** platform operator
**I want to** see structured, machine-readable logs with request IDs for every HTTP request
**So that** I can trace requests through the system and diagnose issues in production

## Acceptance Criteria
- [ ] Every incoming HTTP request is assigned a unique `request_id` (UUID v4) injected as `X-Hive-Request-ID` in both the request (for upstream propagation) and response headers
- [ ] A structured log line is emitted for every request including: `request_id`, `method`, `path`, `status`, `duration_ms`, `user_id` (if authenticated)
- [ ] Log output is NDJSON (one JSON object per line) when `HIVE_LOG_FORMAT=json`; human-readable when `HIVE_LOG_FORMAT=text` (default for development)
- [ ] Log level is controlled by `HIVE_LOG` env var (e.g. `HIVE_LOG=hive_server=debug,info`); default is `info`
- [ ] Sensitive fields (`Authorization` header value, raw API keys) are never logged
- [ ] Spans are used for async operations so that child tasks' logs are correlated to the parent request

## Technical Notes
- Implement using `tracing` + `tracing-subscriber` crates
- Request ID middleware: a `tower` layer that generates the UUID, inserts it as a `RequestId` extension on the `Request`, and appends it to the response headers
- Use `tracing_subscriber::fmt` with a JSON formatter for `HIVE_LOG_FORMAT=json` and the default pretty formatter otherwise; switch at startup based on env var
- `tracing::instrument` on handler functions to create per-request spans; include `request_id` as a span field
- `tower_http::trace::TraceLayer` provides the per-request log line with status and duration; configure it to use the `tracing` backend

## Phase
Cross-cutting
