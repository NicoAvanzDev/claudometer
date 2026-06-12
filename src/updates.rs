use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use once_cell::sync::Lazy;
use serde_json::Value;
use ureq::AgentBuilder;

const UPDATE_CHECK_INTERVAL_MS: u32 = 60 * 60 * 1000;
const LATEST_RELEASE_API_URL: &str =
    "https://api.github.com/repos/NicoAvanzDev/claudometer/releases/latest";

static CHECK_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static SHUTDOWN: AtomicBool = AtomicBool::new(false);
static LAST_CHECK_TICK: AtomicU32 = AtomicU32::new(0);
static CHECK_THREAD: Lazy<Mutex<Option<JoinHandle<()>>>> = Lazy::new(|| Mutex::new(None));

#[derive(Debug)]
struct Release {
    version: String,
    html_url: String,
}

pub fn start_check_if_due(force: bool) {
    if crate::app::is_workstation_locked() {
        crate::diagnostics::log("updates", "check skipped workstation locked");
        return;
    }

    let now = unsafe { windows::Win32::System::SystemInformation::GetTickCount() };
    let last = LAST_CHECK_TICK.load(Ordering::SeqCst);
    if !force && last != 0 && now.wrapping_sub(last) < UPDATE_CHECK_INTERVAL_MS {
        return;
    }

    if CHECK_IN_FLIGHT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        crate::diagnostics::log("updates", "check skipped already in flight");
        return;
    }

    LAST_CHECK_TICK.store(now, Ordering::SeqCst);

    let mut slot = CHECK_THREAD.lock().expect("update thread mutex poisoned");
    if let Some(handle) = slot.take() {
        let _ = handle.join();
    }

    *slot = Some(thread::spawn(|| {
        crate::diagnostics::log("updates", "check started");
        match query_latest_release() {
            Ok(Some(release)) => {
                crate::diagnostics::log(
                    "updates",
                    format!(
                        "update available current={} latest={} url={}",
                        env!("CARGO_PKG_VERSION"),
                        release.version,
                        release.html_url
                    ),
                );
                if !SHUTDOWN.load(Ordering::SeqCst) {
                    crate::tray::show_update_available(&release.version, &release.html_url);
                }
            }
            Ok(None) => crate::diagnostics::log(
                "updates",
                format!("no update available current={}", env!("CARGO_PKG_VERSION")),
            ),
            Err(error) => crate::diagnostics::log("updates", format!("check failed error={error}")),
        }
        CHECK_IN_FLIGHT.store(false, Ordering::SeqCst);
    }));
}

pub fn shutdown() {
    SHUTDOWN.store(true, Ordering::SeqCst);
    if let Some(handle) = CHECK_THREAD
        .lock()
        .expect("update thread mutex poisoned")
        .take()
    {
        let _ = handle.join();
    }
}

fn query_latest_release() -> Result<Option<Release>, String> {
    let tls_connector = ureq::native_tls::TlsConnector::new().map_err(|error| error.to_string())?;
    let agent = AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .tls_connector(Arc::new(tls_connector))
        .build();

    let response = agent
        .get(LATEST_RELEASE_API_URL)
        .set(
            "user-agent",
            concat!("claudometer/", env!("CARGO_PKG_VERSION")),
        )
        .set("accept", "application/vnd.github+json")
        .call()
        .map_err(|error| error.to_string())?;

    let body = response.into_string().map_err(|error| error.to_string())?;
    let json: Value = serde_json::from_str(&body).map_err(|error| error.to_string())?;
    let tag_name = json
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or("latest release response missing tag_name")?;
    let html_url = json
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or("https://github.com/NicoAvanzDev/claudometer/releases/latest");

    if is_newer_version(tag_name, env!("CARGO_PKG_VERSION")) {
        Ok(Some(Release {
            version: tag_name.to_owned(),
            html_url: html_url.to_owned(),
        }))
    } else {
        Ok(None)
    }
}

fn is_newer_version(candidate: &str, current: &str) -> bool {
    normalized_version_parts(candidate) > normalized_version_parts(current)
}

fn normalized_version_parts(version: &str) -> Vec<u32> {
    version
        .trim()
        .trim_start_matches('v')
        .split(|ch: char| !ch.is_ascii_digit())
        .take(3)
        .map(|part| part.parse::<u32>().unwrap_or(0))
        .chain(std::iter::repeat(0))
        .take(3)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::is_newer_version;

    #[test]
    fn compares_release_tags() {
        assert!(is_newer_version("v1.2.1", "1.2.0"));
        assert!(is_newer_version("2.0.0", "1.9.9"));
        assert!(!is_newer_version("v1.2.0", "1.2.0"));
        assert!(!is_newer_version("1.1.9", "1.2.0"));
    }
}
