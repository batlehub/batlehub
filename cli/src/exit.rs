//! One table of exit codes (RFC 0040 §4.6) and the one error shape.
//!
//! A command returns `anyhow::Error`; `main` walks its chain for an
//! [`ApiError`] (a refused HTTP call) or a [`CliError`] (a decision the CLI
//! made itself) and maps it here. Nothing else calls `process::exit`.

use batlehub_core::entities::reason_code_docs_url;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitCode {
    Failure = 1,
    Usage = 2,
    Unauthenticated = 3,
    Refused = 4,
    NotFound = 5,
    Conflict = 6,
    Partial = 7,
    Timeout = 8,
    Confirm = 9,
}

impl ExitCode {
    pub fn from_status(status: u16) -> Self {
        match status {
            401 => Self::Unauthenticated,
            403 => Self::Refused,
            404 => Self::NotFound,
            409 => Self::Conflict,
            _ => Self::Failure,
        }
    }
}

/// A non-success HTTP answer, parsed from the server's `ErrorBody`
/// (`{error, message, code}`) when it sent one.
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
    pub code: Option<String>,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    code: Option<String>,
}

impl ApiError {
    pub fn from_body(status: u16, body: &str) -> Self {
        let parsed = serde_json::from_str::<ErrorBody>(body).ok();
        let code = parsed.as_ref().and_then(|b| b.code.clone());
        let message = parsed
            .and_then(|b| b.message.or(b.error))
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| {
                let body = body.trim();
                if body.is_empty() {
                    format!("the server answered {status}")
                } else {
                    format!("the server answered {status}: {body}")
                }
            });
        Self {
            status,
            message,
            code,
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

/// A failure the CLI decided on itself, with the exit code it means.
#[derive(Debug)]
pub struct CliError {
    pub exit: ExitCode,
    pub message: String,
    pub hint: Option<String>,
    /// The command already wrote its `--format json` result on stdout, so
    /// `main` adds no error document after it: one document per run.
    pub reported: bool,
}

impl CliError {
    pub fn new(exit: ExitCode, message: impl Into<String>) -> Self {
        Self {
            exit,
            message: message.into(),
            hint: None,
            reported: false,
        }
    }

    /// An exit code whose output the command already printed (`why`'s ⛔:
    /// the answer is on stdout, and an `error:` line would repeat it).
    pub fn quiet(exit: ExitCode) -> Self {
        Self::new(exit, "")
    }

    /// A [`quiet`](Self::quiet) exit whose result is already on stdout as the
    /// run's one JSON document (`why`'s ⛔, `audit verify`'s failures).
    pub fn reported(exit: ExitCode) -> Self {
        Self {
            reported: true,
            ..Self::quiet(exit)
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

/// What `main` prints and exits with for an error.
pub struct Report {
    pub exit: u8,
    pub message: String,
    pub code: Option<String>,
    pub docs: Option<String>,
    pub hint: Option<String>,
    /// See [`CliError::reported`].
    pub reported: bool,
}

impl Report {
    pub fn from_error(err: &anyhow::Error) -> Self {
        for cause in err.chain() {
            if let Some(api) = cause.downcast_ref::<ApiError>() {
                return Self {
                    exit: ExitCode::from_status(api.status) as u8,
                    message: api.message.clone(),
                    docs: api.code.as_deref().map(reason_code_docs_url),
                    code: api.code.clone(),
                    hint: (api.status == 401).then(|| "run `batlehub-cli auth login`".to_owned()),
                    reported: false,
                };
            }
            if let Some(cli) = cause.downcast_ref::<CliError>() {
                return Self {
                    exit: cli.exit as u8,
                    message: cli.message.clone(),
                    code: None,
                    docs: None,
                    hint: cli.hint.clone(),
                    reported: cli.reported,
                };
            }
        }
        Self {
            exit: ExitCode::Failure as u8,
            // `{:#}`: the context chain on one line, as anyhow prints it.
            message: format!("{err:#}"),
            code: None,
            docs: None,
            hint: None,
            reported: false,
        }
    }

    /// The stderr block: `error:`, then `code:` and `hint:` when there are any.
    /// Empty for a [`CliError::quiet`] exit.
    pub fn to_stderr(&self) -> String {
        if self.message.is_empty() {
            return String::new();
        }
        let mut out = format!("error: {}", lowercase_first(&self.message));
        if let Some(code) = &self.code {
            out.push_str(&format!("\n  code: {code}"));
            if let Some(docs) = &self.docs {
                out.push_str(&format!(" — {docs}"));
            }
        }
        if let Some(hint) = &self.hint {
            out.push_str(&format!("\nhint: {hint}"));
        }
        out
    }

    /// The `--format json` copy on stdout.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "error": {
            "message": self.message,
            "code": self.code,
            "docs": self.docs,
            "hint": self.hint,
            "exit": self.exit,
        }})
    }
}

/// `Error: Foo` → `foo`, but leave an acronym (`HTTP`, `OIDC`) alone.
fn lowercase_first(s: &str) -> String {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(a), Some(b)) if a.is_uppercase() && !b.is_uppercase() => {
            let mut out = a.to_lowercase().collect::<String>();
            out.push(b);
            out.extend(chars);
            out
        }
        _ => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_map_to_the_table() {
        assert_eq!(ExitCode::from_status(401), ExitCode::Unauthenticated);
        assert_eq!(ExitCode::from_status(403), ExitCode::Refused);
        assert_eq!(ExitCode::from_status(404), ExitCode::NotFound);
        assert_eq!(ExitCode::from_status(409), ExitCode::Conflict);
        assert_eq!(ExitCode::from_status(500), ExitCode::Failure);
        assert_eq!(ExitCode::from_status(400), ExitCode::Failure);
    }

    #[test]
    fn error_body_message_and_code_are_read() {
        let e = ApiError::from_body(
            403,
            r#"{"error":"forbidden","message":"Held until tomorrow","code":"MIN_AGE_NOT_MET"}"#,
        );
        assert_eq!(e.message, "Held until tomorrow");
        assert_eq!(e.code.as_deref(), Some("MIN_AGE_NOT_MET"));
        let r = Report::from_error(&anyhow::Error::new(e).context("yanking"));
        assert_eq!(r.exit, 4);
        let text = r.to_stderr();
        assert!(text.starts_with("error: held until tomorrow"), "{text}");
        assert!(
            text.contains("code: MIN_AGE_NOT_MET — https://") && text.contains("#min-age-not-met"),
            "{text}"
        );
    }

    #[test]
    fn a_non_json_body_is_kept_verbatim() {
        let e = ApiError::from_body(502, "bad gateway");
        assert_eq!(e.message, "the server answered 502: bad gateway");
        assert_eq!(
            ApiError::from_body(500, "").message,
            "the server answered 500"
        );
    }

    #[test]
    fn a_cli_error_carries_its_exit_and_hint() {
        let err =
            anyhow::Error::new(CliError::new(ExitCode::Confirm, "refusing").hint("pass --yes"));
        let r = Report::from_error(&err);
        assert_eq!(r.exit, 9);
        assert_eq!(r.to_stderr(), "error: refusing\nhint: pass --yes");
        assert_eq!(r.to_json()["error"]["exit"], 9);
    }

    #[test]
    fn anything_else_is_one() {
        assert_eq!(Report::from_error(&anyhow::anyhow!("I/O")).exit, 1);
        assert_eq!(lowercase_first("HTTP down"), "HTTP down");
    }
}
