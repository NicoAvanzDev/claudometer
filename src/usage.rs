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

fn build_agent() -> Result<ureq::Agent, ureq::native_tls::Error> {
    let tls_connector = ureq::native_tls::TlsConnector::new()?;
    Ok(AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .tls_connector(Arc::new(tls_connector))
        .build())
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

    ((utilization * 100.0) + 0.5).floor().clamp(0.0, 999.0) as i32
}

fn reset_minutes_from_header(response: &Response, name: &str) -> i32 {
    let Some(value) = response.header(name) else {
        crate::diagnostics::log("usage", format!("missing response header name={name}"));
        return 0;
    };

    let Ok(reset_at) = value.parse::<f64>() else {
        crate::diagnostics::log(
            "usage",
            format!("invalid reset header name={name} value={value}"),
        );
        return 0;
    };

    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return 0;
    };

    let minutes = (reset_at - now.as_secs_f64()) / 60.0;
    if minutes > 0.0 {
        (minutes + 0.5).floor() as i32
    } else {
        0
    }
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
}
