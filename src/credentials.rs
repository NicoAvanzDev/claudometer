use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CLAUDE_OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CLAUDE_OAUTH_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const REFRESH_SKEW_MS: u64 = 10 * 60 * 1000;

struct ClaudeCredential {
    path: PathBuf,
    root: serde_json::Value,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<u64>,
}

pub fn read_claude_token() -> Option<String> {
    read_claude_credential().map(|credential| credential.access_token)
}

pub fn refresh_claude_token_if_due(agent: &ureq::Agent) -> Option<String> {
    let credential = read_claude_credential()?;
    if !credential.should_refresh() {
        crate::diagnostics::log("credentials", "startup token refresh skipped not due");
        return Some(credential.access_token);
    }

    refresh_credential(agent, credential)
}

fn read_claude_credential() -> Option<ClaudeCredential> {
    let candidates = credential_candidates();
    crate::diagnostics::log(
        "credentials",
        format!("checking credential candidates count={}", candidates.len()),
    );

    for path in candidates {
        crate::diagnostics::log(
            "credentials",
            format!("reading candidate path={}", path.display()),
        );

        let blob = match fs::read_to_string(&path) {
            Ok(blob) => {
                crate::diagnostics::log(
                    "credentials",
                    format!(
                        "candidate read ok path={} bytes={}",
                        path.display(),
                        blob.len()
                    ),
                );
                blob
            }
            Err(error) => {
                crate::diagnostics::log(
                    "credentials",
                    format!(
                        "candidate read failed path={} error={error}",
                        path.display()
                    ),
                );
                continue;
            }
        };

        if let Some(credential) = parse_credential(path.clone(), &blob) {
            crate::diagnostics::log(
                "credentials",
                format!(
                    "access token found path={} token_chars={}",
                    path.display(),
                    credential.access_token.chars().count()
                ),
            );
            return Some(credential);
        }

        crate::diagnostics::log(
            "credentials",
            format!("no access token in candidate path={}", path.display()),
        );
    }

    crate::diagnostics::log("credentials", "no usable credential found");
    None
}

fn credential_candidates() -> Vec<PathBuf> {
    if let Ok(path) = env::var("CLAUDE_CREDENTIALS_PATH") {
        if !path.is_empty() {
            crate::diagnostics::log("credentials", "using CLAUDE_CREDENTIALS_PATH");
            return vec![PathBuf::from(path)];
        }
    }

    if let Ok(path) = env::var("CLAUDE_CONFIG_DIR") {
        if !path.is_empty() {
            crate::diagnostics::log("credentials", "using CLAUDE_CONFIG_DIR");
            return vec![PathBuf::from(path).join(".credentials.json")];
        }
    }

    let Some(home) = env::var_os("USERPROFILE") else {
        crate::diagnostics::log("credentials", "USERPROFILE is not set");
        return Vec::new();
    };

    let mut paths = vec![PathBuf::from(&home)
        .join(".claude")
        .join(".credentials.json")];

    if let Ok(local_app_data) = env::var("LOCALAPPDATA") {
        if !local_app_data.is_empty() {
            paths.push(
                PathBuf::from(local_app_data)
                    .join("Claude")
                    .join(".credentials.json"),
            );
        }
    }

    if let Ok(app_data) = env::var("APPDATA") {
        if !app_data.is_empty() {
            paths.push(
                PathBuf::from(app_data)
                    .join("Claude")
                    .join(".credentials.json"),
            );
        }
    }

    paths
}

fn parse_credential(path: PathBuf, blob: &str) -> Option<ClaudeCredential> {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(blob) {
        if let Some(token) = find_access_token(&json) {
            return Some(ClaudeCredential {
                path,
                refresh_token: find_refresh_token(&json).map(str::to_owned),
                expires_at: find_expires_at(&json),
                access_token: token.to_owned(),
                root: json,
            });
        }
    } else {
        crate::diagnostics::log("credentials", "credential content is not json");
    }

    let trimmed = blob.trim();
    if trimmed.len() >= 20
        && trimmed.chars().all(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '~' | '+' | '/' | '=')
        })
    {
        Some(ClaudeCredential {
            path,
            root: serde_json::Value::Null,
            access_token: trimmed.to_owned(),
            refresh_token: None,
            expires_at: None,
        })
    } else {
        None
    }
}

#[cfg(test)]
fn extract_access_token(blob: &str) -> Option<String> {
    parse_credential(PathBuf::new(), blob).map(|credential| credential.access_token)
}

fn find_access_token(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(token) = map.get("accessToken").and_then(|value| value.as_str()) {
                if !token.is_empty() {
                    return Some(token);
                }
            }

            map.values().find_map(find_access_token)
        }
        serde_json::Value::Array(values) => values.iter().find_map(find_access_token),
        _ => None,
    }
}

fn find_refresh_token(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(token) = map.get("refreshToken").and_then(|value| value.as_str()) {
                if !token.is_empty() {
                    return Some(token);
                }
            }

            map.values().find_map(find_refresh_token)
        }
        serde_json::Value::Array(values) => values.iter().find_map(find_refresh_token),
        _ => None,
    }
}

fn find_expires_at(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(expires_at) = map.get("expiresAt").and_then(json_u64) {
                return Some(expires_at);
            }

            map.values().find_map(find_expires_at)
        }
        serde_json::Value::Array(values) => values.iter().find_map(find_expires_at),
        _ => None,
    }
}

fn json_u64(value: &serde_json::Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|value| *value >= 0.0)
            .map(|value| value as u64)
    })
}

impl ClaudeCredential {
    fn should_refresh(&self) -> bool {
        let Some(expires_at) = self.expires_at else {
            return false;
        };

        expires_at <= now_ms().saturating_add(REFRESH_SKEW_MS)
    }
}

fn refresh_credential(agent: &ureq::Agent, mut credential: ClaudeCredential) -> Option<String> {
    let Some(refresh_token) = credential.refresh_token.as_deref() else {
        crate::diagnostics::log("credentials", "token refresh skipped no refresh token");
        return None;
    };

    crate::diagnostics::log("credentials", "token refresh start");
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLAUDE_OAUTH_CLIENT_ID,
    })
    .to_string();

    let response = match agent
        .post(CLAUDE_OAUTH_TOKEN_URL)
        .set("content-type", "application/json")
        .set("accept", "application/json")
        .set("user-agent", "claude-code/2.1.5")
        .send_string(&body)
    {
        Ok(response) => response,
        Err(ureq::Error::Status(code, response)) => {
            crate::diagnostics::log(
                "credentials",
                format!("token refresh rejected status={code}"),
            );
            let _ = response.into_string();
            return None;
        }
        Err(error) => {
            crate::diagnostics::log("credentials", format!("token refresh failed error={error}"));
            return None;
        }
    };

    let Ok(response_body) = response.into_string() else {
        crate::diagnostics::log("credentials", "token refresh failed reading response");
        return None;
    };

    let Ok(response_json) = serde_json::from_str::<serde_json::Value>(&response_body) else {
        crate::diagnostics::log("credentials", "token refresh response is not json");
        return None;
    };

    let Some(access_token) = response_json
        .get("access_token")
        .or_else(|| response_json.get("accessToken"))
        .and_then(|value| value.as_str())
        .filter(|token| !token.is_empty())
    else {
        crate::diagnostics::log("credentials", "token refresh response missing access token");
        return None;
    };

    let new_refresh_token = response_json
        .get("refresh_token")
        .or_else(|| response_json.get("refreshToken"))
        .and_then(|value| value.as_str())
        .filter(|token| !token.is_empty())
        .unwrap_or(refresh_token);
    let expires_at = refreshed_expires_at(&response_json);

    update_credential_json(
        &mut credential.root,
        access_token,
        new_refresh_token,
        expires_at,
    );

    let Ok(updated) = serde_json::to_string(&credential.root) else {
        crate::diagnostics::log(
            "credentials",
            "token refresh failed serializing credentials",
        );
        return None;
    };

    if let Err(error) = fs::write(&credential.path, updated) {
        crate::diagnostics::log(
            "credentials",
            format!(
                "token refresh failed writing path={} error={error}",
                credential.path.display()
            ),
        );
        return None;
    }

    crate::diagnostics::log(
        "credentials",
        format!(
            "token refresh ok path={} token_chars={}",
            credential.path.display(),
            access_token.chars().count()
        ),
    );
    Some(access_token.to_owned())
}

fn refreshed_expires_at(response: &serde_json::Value) -> u64 {
    response
        .get("expires_at")
        .or_else(|| response.get("expiresAt"))
        .and_then(json_u64)
        .unwrap_or_else(|| {
            let expires_in_ms = response
                .get("expires_in")
                .or_else(|| response.get("expiresIn"))
                .and_then(json_u64)
                .unwrap_or(3600)
                .saturating_mul(1000);
            now_ms().saturating_add(expires_in_ms)
        })
}

fn update_credential_json(
    root: &mut serde_json::Value,
    access_token: &str,
    refresh_token: &str,
    expires_at: u64,
) {
    if !root.is_object() {
        *root = serde_json::json!({});
    }

    let oauth = root
        .as_object_mut()
        .expect("credential root should be an object")
        .entry("claudeAiOauth")
        .or_insert_with(|| serde_json::json!({}));

    if !oauth.is_object() {
        *oauth = serde_json::json!({});
    }

    let map = oauth
        .as_object_mut()
        .expect("claudeAiOauth should be an object");
    map.insert(
        "accessToken".to_owned(),
        serde_json::Value::String(access_token.to_owned()),
    );
    map.insert(
        "refreshToken".to_owned(),
        serde_json::Value::String(refresh_token.to_owned()),
    );
    map.insert(
        "expiresAt".to_owned(),
        serde_json::Value::Number(serde_json::Number::from(expires_at)),
    );
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::extract_access_token;

    #[test]
    fn reads_nested_claude_oauth_token() {
        let token = "abcDE12345abcDE12345abcDE12345";
        let blob = format!(r#"{{"claudeAiOauth":{{"accessToken":"{token}"}}}}"#);

        assert_eq!(extract_access_token(&blob).as_deref(), Some(token));
    }

    #[test]
    fn reads_top_level_access_token() {
        let token = "abcDE12345abcDE12345abcDE12345";
        let blob = format!(r#"{{"accessToken":"{token}"}}"#);

        assert_eq!(extract_access_token(&blob).as_deref(), Some(token));
    }

    #[test]
    fn reads_raw_token_file() {
        let token = "abcDE12345abcDE12345abcDE12345";

        assert_eq!(extract_access_token(token).as_deref(), Some(token));
    }
}
