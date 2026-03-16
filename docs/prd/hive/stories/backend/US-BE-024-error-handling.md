# US-BE-024: Error handling

**As a** Hive frontend developer
**I want to** receive structured JSON error responses from all API endpoints
**So that** I can display meaningful error messages to users and handle errors programmatically

## Acceptance Criteria
- [ ] All error responses return a JSON body with at minimum `{"error": "<snake_case_code>", "message": "<human readable>"}` and the appropriate HTTP status code
- [ ] Validation errors include a `"fields"` array: `[{"field": "name", "message": "must not be empty"}]`
- [ ] Unhandled panics are caught and return `500 Internal Server Error` with `{"error": "internal_error"}` (no stack trace in response)
- [ ] `404` responses for unknown routes return `{"error": "not_found"}` rather than axum's default plain-text response
- [ ] `405 Method Not Allowed` returns `{"error": "method_not_allowed"}`
- [ ] Every error response includes a `"request_id"` field matching the `X-Hive-Request-ID` header (see US-BE-025)
- [ ] Error codes are documented in a `docs/hive-server/errors.md` reference

## Technical Notes
- Implement as an axum `IntoResponse` impl on a central `AppError` enum in `crates/hive-server/src/error.rs`
- `AppError` variants map to HTTP status codes: `NotFound` → 404, `Forbidden` → 403, `Conflict` → 409, `Validation` → 400/422, `DaemonUnavailable` → 502, `Internal` → 500
- Use axum's `HandleErrorLayer` for panic catching via `tower::ServiceBuilder`
- Custom 404/405 handlers registered via `axum::Router::fallback` and method routing
- The `"request_id"` field is injected by the logging middleware (US-BE-025) which stores the ID in a `RequestId` extension; `AppError::into_response` reads it from the extensions

## Phase
Cross-cutting
