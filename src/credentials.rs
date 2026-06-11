use std::env;
use std::fs;
use std::path::PathBuf;

pub fn read_claude_token() -> Option<String> {
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

        if let Some(token) = extract_access_token(&blob) {
            crate::diagnostics::log(
                "credentials",
                format!(
                    "access token found path={} token_chars={}",
                    path.display(),
                    token.chars().count()
                ),
            );
            return Some(token);
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

fn extract_access_token(blob: &str) -> Option<String> {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(blob) {
        if let Some(token) = find_access_token(&json) {
            return Some(token.to_owned());
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
        Some(trimmed.to_owned())
    } else {
        None
    }
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
