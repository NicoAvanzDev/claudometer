use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use once_cell::sync::Lazy;
use ureq::{AgentBuilder, Response};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::credentials;
use crate::widget;

pub const USAGE_POLL_INTERVAL_MS: u32 = 5 * 60 * 1000;
pub const TIMER_INTERVAL_MS: u32 = 60 * 1000;

#[derive(Clone)]
pub struct UsageSnapshot {
    pub session_percent: i32,
    pub weekly_percent: i32,
    pub session_reset_minutes: i32,
    pub weekly_reset_minutes: i32,
    pub status: String,
    pub ok: bool,
}

impl Default for UsageSnapshot {
    fn default() -> Self {
        Self {
            session_percent: 0,
            weekly_percent: 0,
            session_reset_minutes: 0,
            weekly_reset_minutes: 0,
            status: "starting".to_owned(),
            ok: false,
        }
    }
}

pub static USAGE: Lazy<Mutex<UsageSnapshot>> = Lazy::new(|| Mutex::new(UsageSnapshot::default()));

static FETCH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static SHUTDOWN: AtomicBool = AtomicBool::new(false);
static LAST_FETCH_TICK: AtomicU32 = AtomicU32::new(0);
static FETCH_THREAD: Lazy<Mutex<Option<JoinHandle<()>>>> = Lazy::new(|| Mutex::new(None));

pub fn snapshot() -> UsageSnapshot {
    USAGE.lock().expect("usage mutex poisoned").clone()
}

pub fn session_reset_label(minutes: i32) -> Option<String> {
    if minutes <= 0 {
        return None;
    }

    if minutes < 60 {
        return Some(format!("{minutes}m"));
    }

    Some(format!("{}h", ((minutes + 30) / 60).max(1)))
}

pub fn weekly_reset_label(minutes: i32) -> Option<String> {
    if minutes <= 0 {
        return None;
    }

    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    let reset_at = now.as_secs().saturating_add(minutes as u64 * 60);
    Some(weekday_label_from_unix_seconds(reset_at).to_owned())
}

pub fn refresh_auth_on_startup() {
    let agent = match build_agent() {
        Ok(agent) => agent,
        Err(_) => {
            crate::diagnostics::log("usage", "startup auth refresh skipped http init failed");
            return;
        }
    };

    if credentials::refresh_claude_token_if_due(&agent).is_some() {
        crate::diagnostics::log("usage", "startup auth refresh check finished");
    } else {
        crate::diagnostics::log("usage", "startup auth refresh check failed");
    }
}

pub fn start_fetch_if_due(force: bool) {
    if crate::app::is_workstation_locked() {
        crate::diagnostics::log("usage", "fetch skipped workstation locked");
        return;
    }

    let now = unsafe { windows::Win32::System::SystemInformation::GetTickCount() };
    let last = LAST_FETCH_TICK.load(Ordering::SeqCst);
    if !force && last != 0 && now.wrapping_sub(last) < USAGE_POLL_INTERVAL_MS {
        crate::diagnostics::log(
            "usage",
            format!(
                "fetch skipped not due elapsed_ms={}",
                now.wrapping_sub(last)
            ),
        );
        return;
    }

    if FETCH_IN_FLIGHT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        crate::diagnostics::log("usage", "fetch skipped already in flight");
        return;
    }

    LAST_FETCH_TICK.store(now, Ordering::SeqCst);

    let mut slot = FETCH_THREAD.lock().expect("fetch thread mutex poisoned");
    if let Some(handle) = slot.take() {
        let _ = handle.join();
    }

    *slot = Some(thread::spawn(|| {
        crate::diagnostics::log("usage", "fetch started");
        let mut snapshot = UsageSnapshot::default();
        let _ = query_claude_usage(&mut snapshot);
        crate::diagnostics::log(
            "usage",
            format!(
                "fetch finished ok={} status={} session_percent={} weekly_percent={} session_reset_minutes={} weekly_reset_minutes={}",
                snapshot.ok,
                snapshot.status,
                snapshot.session_percent,
                snapshot.weekly_percent,
                snapshot.session_reset_minutes,
                snapshot.weekly_reset_minutes
            ),
        );
        update_usage_state(snapshot);
        FETCH_IN_FLIGHT.store(false, Ordering::SeqCst);
    }));
}

pub fn shutdown() {
    SHUTDOWN.store(true, Ordering::SeqCst);
    if let Some(handle) = FETCH_THREAD
        .lock()
        .expect("fetch thread mutex poisoned")
        .take()
    {
        let _ = handle.join();
    }
}

fn update_usage_state(snapshot: UsageSnapshot) {
    *USAGE.lock().expect("usage mutex poisoned") = snapshot;

    if SHUTDOWN.load(Ordering::SeqCst) {
        return;
    }

    for hwnd in widget::widget_hwnds() {
        unsafe {
            let _ = PostMessageW(hwnd, widget::WM_USAGE_UPDATED, WPARAM(0), LPARAM(0));
        }
    }
}

fn query_claude_usage(snapshot: &mut UsageSnapshot) -> bool {
    let agent = match build_agent() {
        Ok(agent) => agent,
        Err(_) => {
            snapshot.status = "http init".to_owned();
            snapshot.ok = false;
            crate::diagnostics::log("usage", "http client initialization failed");
            return false;
        }
    };

    let Some(token) = credentials::read_claude_token() else {
        snapshot.status = "no token".to_owned();
        snapshot.ok = false;
        crate::diagnostics::log("usage", "query stopped no token");
        return false;
    };
    crate::diagnostics::log(
        "usage",
        format!("token loaded token_chars={}", token.chars().count()),
    );

    let response = match send_usage_request(&agent, &token) {
        UsageResponse::Ok(response) => response,
        UsageResponse::RateLimited(response) => {
            apply_rate_limit_snapshot(snapshot, &response);
            log_response_headers(&response);
            return true;
        }
        UsageResponse::AuthRejected => {
            snapshot.status = "login".to_owned();
            snapshot.ok = false;
            return false;
        }
        UsageResponse::HttpError(status) => {
            snapshot.status = format!("api {status}");
            snapshot.ok = false;
            return false;
        }
        UsageResponse::Failed => {
            snapshot.status = "http failed".to_owned();
            snapshot.ok = false;
            return false;
        }
    };

    snapshot.session_percent =
        percent_from_header(&response, "anthropic-ratelimit-unified-5h-utilization");
    snapshot.weekly_percent =
        percent_from_header(&response, "anthropic-ratelimit-unified-7d-utilization");
    snapshot.session_reset_minutes =
        reset_minutes_from_header(&response, "anthropic-ratelimit-unified-5h-reset");
    snapshot.weekly_reset_minutes =
        reset_minutes_from_header(&response, "anthropic-ratelimit-unified-7d-reset");
    snapshot.status = response
        .header("anthropic-ratelimit-unified-5h-status")
        .unwrap_or("ok")
        .to_owned();
    snapshot.ok = true;
    log_response_headers(&response);
    true
}

enum UsageResponse {
    Ok(Response),
    RateLimited(Response),
    AuthRejected,
    HttpError(u16),
    Failed,
}

fn send_usage_request(agent: &ureq::Agent, token: &str) -> UsageResponse {
    let body = r#"{"model":"claude-haiku-4-5-20251001","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}"#;
    crate::diagnostics::log(
        "usage",
        "api request start method=POST url=https://api.anthropic.com/v1/messages model=claude-haiku-4-5-20251001",
    );
    let response = agent
        .post("https://api.anthropic.com/v1/messages")
        .set("anthropic-version", "2023-06-01")
        .set("anthropic-beta", "oauth-2025-04-20")
        .set("content-type", "application/json")
        .set("user-agent", "claude-code/2.1.5")
        .set("authorization", &format!("Bearer {token}"))
        .send_string(body);

    match response {
        Ok(response) => {
            crate::diagnostics::log(
                "usage",
                format!("api response ok status={}", response.status()),
            );
            UsageResponse::Ok(response)
        }
        Err(ureq::Error::Status(code, response)) => {
            crate::diagnostics::log("usage", format!("api response error status={code}"));
            if code == 401 || code == 403 {
                UsageResponse::AuthRejected
            } else if code == 429 {
                UsageResponse::RateLimited(response)
            } else {
                let _ = response.into_string();
                UsageResponse::HttpError(code)
            }
        }
        Err(error) => {
            crate::diagnostics::log("usage", format!("api request failed error={error}"));
            UsageResponse::Failed
        }
    }
}

fn apply_rate_limit_snapshot(snapshot: &mut UsageSnapshot, response: &Response) {
    snapshot.session_percent =
        percent_from_header_or(response, "anthropic-ratelimit-unified-5h-utilization", 100);
    snapshot.weekly_percent =
        percent_from_header_or(response, "anthropic-ratelimit-unified-7d-utilization", 0);
    snapshot.session_reset_minutes = retry_after_minutes(response)
        .or_else(|| {
            reset_minutes_from_first_header(
                response,
                &[
                    "anthropic-ratelimit-unified-5h-reset",
                    "anthropic-ratelimit-unified-reset",
                ],
            )
        })
        .unwrap_or(0);
    snapshot.weekly_reset_minutes =
        reset_minutes_from_header(response, "anthropic-ratelimit-unified-7d-reset");
    snapshot.status = response
        .header("anthropic-ratelimit-unified-5h-status")
        .unwrap_or("limited")
        .to_owned();
    snapshot.ok = true;
}

fn build_agent() -> Result<ureq::Agent, ureq::native_tls::Error> {
    let tls_connector = ureq::native_tls::TlsConnector::new()?;
    Ok(AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .tls_connector(Arc::new(tls_connector))
        .build())
}

fn percent_from_header_or(response: &Response, name: &str, fallback: i32) -> i32 {
    let Some(value) = response.header(name) else {
        crate::diagnostics::log("usage", format!("missing response header name={name}"));
        return fallback;
    };

    let Ok(utilization) = value.parse::<f64>() else {
        crate::diagnostics::log(
            "usage",
            format!("invalid percent header name={name} value={value}"),
        );
        return fallback;
    };

    percent_from_utilization(utilization)
}

fn percent_from_header(response: &Response, name: &str) -> i32 {
    let Some(value) = response.header(name) else {
        crate::diagnostics::log("usage", format!("missing response header name={name}"));
        return 0;
    };

    let Ok(utilization) = value.parse::<f64>() else {
        crate::diagnostics::log(
            "usage",
            format!("invalid percent header name={name} value={value}"),
        );
        return 0;
    };

    percent_from_utilization(utilization)
}

fn percent_from_utilization(utilization: f64) -> i32 {
    ((utilization * 100.0) + 0.5).floor().clamp(0.0, 999.0) as i32
}

fn reset_minutes_from_header(response: &Response, name: &str) -> i32 {
    let Some(value) = response.header(name) else {
        crate::diagnostics::log("usage", format!("missing response header name={name}"));
        return 0;
    };

    reset_minutes_from_value(name, value).unwrap_or(0)
}

fn reset_minutes_from_first_header(response: &Response, names: &[&str]) -> Option<i32> {
    for name in names {
        if let Some(value) = response.header(name) {
            return reset_minutes_from_value(name, value);
        }
    }

    crate::diagnostics::log(
        "usage",
        format!("missing response headers names={}", names.join(",")),
    );
    None
}

fn reset_minutes_from_value(name: &str, value: &str) -> Option<i32> {
    let Some(reset_at) = unix_seconds_from_reset_value(value) else {
        crate::diagnostics::log(
            "usage",
            format!("invalid reset header name={name} value={value}"),
        );
        return None;
    };

    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return None;
    };

    let minutes = (reset_at as f64 - now.as_secs_f64()) / 60.0;
    if minutes > 0.0 {
        Some((minutes + 0.5).floor() as i32)
    } else {
        Some(0)
    }
}

fn retry_after_minutes(response: &Response) -> Option<i32> {
    let value = response.header("retry-after")?;
    retry_after_minutes_from_value(value)
}

fn retry_after_minutes_from_value(value: &str) -> Option<i32> {
    let seconds = value.parse::<f64>().ok()?;
    if seconds > 0.0 {
        Some((seconds / 60.0 + 0.5).floor().max(1.0) as i32)
    } else {
        Some(0)
    }
}

fn unix_seconds_from_reset_value(value: &str) -> Option<i64> {
    if let Ok(seconds) = value.parse::<f64>() {
        return Some(seconds as i64);
    }

    unix_seconds_from_rfc3339(value)
}

fn unix_seconds_from_rfc3339(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 20 {
        return None;
    }

    let year = parse_digits(value, 0, 4)?;
    let month = parse_digits(value, 5, 7)?;
    let day = parse_digits(value, 8, 10)?;
    let hour = parse_digits(value, 11, 13)?;
    let minute = parse_digits(value, 14, 16)?;
    let second = parse_digits(value, 17, 19)?;

    if bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !matches!(bytes.get(10), Some(b'T') | Some(b't'))
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }

    let mut index = 19;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let fraction_start = index;
        while matches!(bytes.get(index), Some(b'0'..=b'9')) {
            index += 1;
        }
        if index == fraction_start {
            return None;
        }
    }

    let offset_seconds = match bytes.get(index) {
        Some(b'Z') | Some(b'z') if index + 1 == bytes.len() => 0,
        Some(b'+') | Some(b'-') if index + 6 == bytes.len() => {
            let sign = if bytes[index] == b'+' { 1 } else { -1 };
            if bytes.get(index + 3) != Some(&b':') {
                return None;
            }
            let offset_hour = parse_digits(value, index + 1, index + 3)?;
            let offset_minute = parse_digits(value, index + 4, index + 6)?;
            if offset_hour > 23 || offset_minute > 59 {
                return None;
            }
            sign * ((offset_hour * 60 + offset_minute) * 60)
        }
        _ => return None,
    };

    let days = days_from_civil(year, month, day)?;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second - offset_seconds)
}

fn parse_digits(value: &str, start: usize, end: usize) -> Option<i64> {
    value.get(start..end)?.parse().ok()
}

fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    const DAYS_BEFORE_MONTH: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return None,
    };

    if day < 1 || day > days_in_month {
        return None;
    }

    let years_before = year - 1;
    let leap_days_before_year = years_before / 4 - years_before / 100 + years_before / 400;
    let days_before_year = years_before * 365 + leap_days_before_year;
    let leap_day = if month > 2 && is_leap_year(year) {
        1
    } else {
        0
    };
    let days_since_year_zero =
        days_before_year + DAYS_BEFORE_MONTH[(month - 1) as usize] + leap_day + day - 1;

    Some(days_since_year_zero - 719_162)
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn weekday_label_from_unix_seconds(seconds: u64) -> &'static str {
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let days_since_epoch = seconds / 86_400;
    let sunday_based_index = (days_since_epoch + 4) % 7;
    WEEKDAYS[sunday_based_index as usize]
}

fn log_response_headers(response: &Response) {
    crate::diagnostics::log(
        "usage",
        format!(
            "rate headers 5h_utilization={} 7d_utilization={} 5h_reset={} 7d_reset={} 5h_status={}",
            response
                .header("anthropic-ratelimit-unified-5h-utilization")
                .unwrap_or("<missing>"),
            response
                .header("anthropic-ratelimit-unified-7d-utilization")
                .unwrap_or("<missing>"),
            response
                .header("anthropic-ratelimit-unified-5h-reset")
                .unwrap_or("<missing>"),
            response
                .header("anthropic-ratelimit-unified-7d-reset")
                .unwrap_or("<missing>"),
            response
                .header("anthropic-ratelimit-unified-5h-status")
                .unwrap_or("<missing>")
        ),
    );
    crate::diagnostics::log(
        "usage",
        format!(
            "retry headers retry_after={} requests_limit={}",
            response.header("retry-after").unwrap_or("<missing>"),
            response
                .header("anthropic-ratelimit-requests-limit")
                .unwrap_or("<missing>")
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::{
        retry_after_minutes_from_value, session_reset_label, unix_seconds_from_reset_value,
        weekday_label_from_unix_seconds,
    };

    #[test]
    fn formats_session_reset_compactly() {
        assert_eq!(session_reset_label(0), None);
        assert_eq!(session_reset_label(12), Some("12m".to_owned()));
        assert_eq!(session_reset_label(89), Some("1h".to_owned()));
        assert_eq!(session_reset_label(90), Some("2h".to_owned()));
    }

    #[test]
    fn derives_weekday_from_unix_seconds() {
        assert_eq!(weekday_label_from_unix_seconds(0), "Thu");
        assert_eq!(weekday_label_from_unix_seconds(3 * 86_400), "Sun");
    }

    #[test]
    fn parses_reset_values() {
        assert_eq!(
            unix_seconds_from_reset_value("1764554400"),
            Some(1_764_554_400)
        );
        assert_eq!(
            unix_seconds_from_reset_value("2025-12-01T06:00:00Z"),
            Some(1_764_568_800)
        );
        assert_eq!(
            unix_seconds_from_reset_value("2025-12-01T07:00:00+01:00"),
            Some(1_764_568_800)
        );
    }

    #[test]
    fn parses_retry_after_minutes() {
        assert_eq!(retry_after_minutes_from_value("0"), Some(0));
        assert_eq!(retry_after_minutes_from_value("1"), Some(1));
        assert_eq!(retry_after_minutes_from_value("89"), Some(1));
        assert_eq!(retry_after_minutes_from_value("90"), Some(2));
    }
}
