//! Dictation for the phone composer. Audio is decoded by the local macOS
//! helper and the result goes back to the draft, independently of terminal I/O.

use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use tokio::process::Command;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::timeout;

use crate::api::{authed, error, AppState};

const MAX_SECONDS: usize = 30;
const MAX_SAMPLE_RATE: usize = 192_000;
const WAV_HEADER: usize = 44;
const MAX_BODY: usize = WAV_HEADER + MAX_SECONDS * MAX_SAMPLE_RATE * 2;
const AVAILABILITY_TTL: Duration = Duration::from_secs(30);

pub(crate) struct Voice {
    helper: Option<PathBuf>,
    availability: Mutex<Option<(Instant, bool)>>,
    slot: Semaphore,
}

impl Voice {
    pub fn new() -> Self {
        let helper = std::env::current_exe().ok().and_then(|exe| {
            let dir = exe.parent()?;
            // A signed bundle has a fixed helper path. Standalone dev builds
            // keep the helper next to the binary.
            let helper = if dir.file_name().is_some_and(|name| name == "MacOS") {
                dir.parent()?.join("Helpers/oo-voice")
            } else {
                dir.join("oo-voice")
            };
            helper.is_file().then_some(helper)
        });
        Self {
            helper,
            availability: Mutex::new(None),
            slot: Semaphore::new(1),
        }
    }

    pub async fn available(&self) -> bool {
        let Some(helper) = &self.helper else {
            return false;
        };
        let mut cached = self.availability.lock().await;
        if let Some((_, available)) =
            cached.filter(|(checked, _)| checked.elapsed() < AVAILABILITY_TTL)
        {
            return available;
        }
        let available = timeout(
            Duration::from_secs(4),
            Command::new(helper)
                .arg("--available")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await
        .is_ok_and(|result| result.is_ok_and(|status| status.success()));
        *cached = Some((Instant::now(), available));
        available
    }

    #[cfg(test)]
    pub(crate) fn use_test_helper(&mut self, helper: PathBuf) {
        self.helper = Some(helper);
    }
}

// The web recorder sends a canonical, mono 16-bit PCM WAV. Check the declared
// rate, size and duration before passing any data to a native decoder.
fn valid_wav(bytes: &[u8]) -> bool {
    if bytes.len() <= WAV_HEADER
        || &bytes[..4] != b"RIFF"
        || &bytes[8..16] != b"WAVEfmt "
        || &bytes[16..24] != b"\x10\0\0\0\x01\0\x01\0"
        || &bytes[32..40] != b"\x02\0\x10\0data"
    {
        return false;
    }
    let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
    let rate = word(24);
    let data_len = bytes.len() - WAV_HEADER;
    (8_000..=MAX_SAMPLE_RATE).contains(&rate)
        && word(4) == bytes.len() - 8
        && word(28) == rate * 2
        && word(40) == data_len
        && data_len.is_multiple_of(2)
        && data_len <= rate * MAX_SECONDS * 2
}

pub(crate) async fn transcribe(State(s): AppState, request: Request) -> Response {
    let (device, _) = match authed(&s, request.headers()) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if device.read_only {
        return error(StatusCode::FORBIDDEN, "read_only");
    }
    if request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        != Some("audio/wav")
    {
        return error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "invalid_audio");
    }
    let Some(helper) = s.voice.helper.as_ref() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "voice_unavailable");
    };
    if !s.voice.available().await {
        return error(StatusCode::SERVICE_UNAVAILABLE, "voice_unavailable");
    }
    let Ok(_permit) = s.voice.slot.try_acquire() else {
        return error(StatusCode::TOO_MANY_REQUESTS, "voice_busy");
    };
    let bytes = match timeout(
        Duration::from_secs(15),
        axum::body::to_bytes(request.into_body(), MAX_BODY),
    )
    .await
    {
        Ok(Ok(bytes)) if valid_wav(&bytes) => bytes,
        Ok(Ok(_)) => return error(StatusCode::UNPROCESSABLE_ENTITY, "invalid_audio"),
        Ok(Err(_)) => return error(StatusCode::PAYLOAD_TOO_LARGE, "too_large"),
        Err(_) => return error(StatusCode::REQUEST_TIMEOUT, "voice_timeout"),
    };
    let Ok(Ok(file)) = tokio::task::spawn_blocking(move || {
        let mut file = tempfile::Builder::new().suffix(".wav").tempfile()?;
        file.write_all(&bytes)?;
        Ok::<_, std::io::Error>(file)
    })
    .await
    else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "voice_failed");
    };
    let mut shutdown = s.shutdown.subscribe();
    let result = tokio::select! {
        _ = shutdown.wait_for(|stopped| *stopped) => {
            return error(StatusCode::SERVICE_UNAVAILABLE, "voice_unavailable");
        }
        result = timeout(Duration::from_secs(120), Command::new(helper)
            .arg(file.path())
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output()) => result,
    };
    let Ok(result) = result else {
        return error(StatusCode::GATEWAY_TIMEOUT, "voice_timeout");
    };
    let Ok(output) = result else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "voice_failed");
    };
    match output.status.code() {
        Some(0) => {}
        Some(2) => return error(StatusCode::SERVICE_UNAVAILABLE, "voice_unavailable"),
        Some(4) => return error(StatusCode::SERVICE_UNAVAILABLE, "voice_model_unavailable"),
        _ => return error(StatusCode::SERVICE_UNAVAILABLE, "voice_failed"),
    }
    if !s
        .current_file()
        .devices
        .iter()
        .any(|d| d.id == device.id && !d.read_only)
    {
        return error(StatusCode::FORBIDDEN, "read_only");
    }
    let text = String::from_utf8_lossy(&output.stdout);
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "text": text.trim() })),
    )
        .into_response()
}
