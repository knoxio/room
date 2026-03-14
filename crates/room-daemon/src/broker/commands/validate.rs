use crate::plugin::{CommandInfo, ParamType};

/// Validate `params` against a command's [`CommandInfo`] schema.
///
/// Returns `Ok(())` if all constraints pass, or `Err(message)` with a
/// human-readable error suitable for sending back as a reply.
///
/// Validation rules:
/// - Required params must be present (not blank).
/// - `ParamType::Choice` values must be in the allowed set.
/// - `ParamType::Number` values must parse as `i64` and respect min/max bounds.
/// - `ParamType::Text` and `ParamType::Username` are accepted as-is (no
///   server-side validation — username existence is not checked here).
pub(super) fn validate_params(params: &[String], schema: &CommandInfo) -> Result<(), String> {
    for (i, ps) in schema.params.iter().enumerate() {
        let value = params.get(i).map(String::as_str).unwrap_or("");
        if ps.required && value.is_empty() {
            return Err(format!(
                "/{}: missing required parameter <{}>",
                schema.name, ps.name
            ));
        }
        if value.is_empty() {
            continue;
        }
        match &ps.param_type {
            ParamType::Choice(allowed) => {
                if !allowed.iter().any(|a| a == value) {
                    return Err(format!(
                        "/{}: <{}> must be one of: {}",
                        schema.name,
                        ps.name,
                        allowed.join(", ")
                    ));
                }
            }
            ParamType::Number { min, max } => {
                let Ok(n) = value.parse::<i64>() else {
                    return Err(format!(
                        "/{}: <{}> must be a number, got '{}'",
                        schema.name, ps.name, value
                    ));
                };
                if let Some(lo) = min {
                    if n < *lo {
                        return Err(format!("/{}: <{}> must be >= {lo}", schema.name, ps.name));
                    }
                }
                if let Some(hi) = max {
                    if n > *hi {
                        return Err(format!("/{}: <{}> must be <= {hi}", schema.name, ps.name));
                    }
                }
            }
            ParamType::Text | ParamType::Username => {}
        }
    }
    Ok(())
}
