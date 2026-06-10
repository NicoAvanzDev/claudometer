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

pub fn start_fetch_if_due(force: bool) {
    if crate::app::is_workstation_locked() {
        return;
    }

    let now = unsafe { windows::Win32::System::SystemInformation::GetTickCount() };
    let last = LAST_FETCH_TICK.load(Ordering::SeqCst);
    if !force && last != 0 && now.wrapping_sub(last) < USAGE_POLL_INTERVAL_MS {
        return;
    }

    if FETCH_IN_FLIGHT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    LAST_FETCH_TICK.store(now, Ordering::SeqCst);

    let mut slot = FETCH_THREAD.lock().expect("fetch thread mutex poisoned");
    if let Some(handle) = slot.take() {
        let _ = handle.join();
    }

    *slot = Some(thread::spawn(|| {
        let mut snapshot = UsageSnapshot::default();
        let _ = query_claude_usage(&mut snapshot);
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
    let Some(token) = credentials::read_claude_token() else {
        snapshot.status = "no token".to_owned();
        snapshot.ok = false;
        return false;
    };

    let agent = match build_agent() {
        Ok(agent) => agent,
        Err(_) => {
            snapshot.status = "http init".to_owned();
            snapshot.ok = false;
            return false;
        }
    };

    let body = r#"{"model":"claude-haiku-4-5-20251001","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}"#;
    let response = agent
        .post("https://api.anthropic.com/v1/messages")
        .set("anthropic-version", "2023-06-01")
        .set("anthropic-beta", "oauth-2025-04-20")
        .set("content-type", "application/json")
        .set("user-agent", "claude-code/2.1.5")
        .set("authorization", &format!("Bearer {token}"))
        .send_string(body);

    let response = match response {
        Ok(response) => response,
        Err(ureq::Error::Status(code, response)) => {
            if code == 401 || code == 403 {
                snapshot.status = "login".to_owned();
            } else {
                snapshot.status = format!("api {code}");
            }
            snapshot.ok = false;
            return response.status() < 400;
        }
        Err(_) => {
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
    true
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
        return 0;
    };

    let Ok(utilization) = value.parse::<f64>() else {
        return 0;
    };

    ((utilization * 100.0) + 0.5).floor().clamp(0.0, 999.0) as i32
}

fn reset_minutes_from_header(response: &Response, name: &str) -> i32 {
    let Some(value) = response.header(name) else {
        return 0;
    };

    let Ok(reset_at) = value.parse::<f64>() else {
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
