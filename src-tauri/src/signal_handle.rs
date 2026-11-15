use crate::managers::meetings::MeetingManager;
use crate::TranscriptionCoordinator;
#[cfg(unix)]
use log::debug;
use log::warn;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

#[cfg(target_os = "macos")]
use signal_hook::consts::SIGUSR1;
#[cfg(unix)]
use signal_hook::consts::{SIGTERM, SIGUSR2};
#[cfg(unix)]
use signal_hook::iterator::Signals;
#[cfg(unix)]
use std::thread;

/// Send a transcription input to the coordinator.
/// Used by signal handlers, CLI flags, and any other external trigger.
pub fn send_transcription_input(app: &AppHandle, binding_id: &str, source: &str) {
    if let Some(c) = app.try_state::<TranscriptionCoordinator>() {
        c.send_external_input(binding_id, source);
    } else {
        warn!("TranscriptionCoordinator not initialized");
    }
}

/// Start or stop a meeting, off the calling thread: it opens audio devices.
/// Used by the CLI flag and the tray.
pub fn toggle_meeting(app: &AppHandle) {
    change_meeting(app, MeetingManager::toggle);
}

/// Start or stop a meeting as the user asked, off the calling thread. Used by
/// the Meetings page and the overlay, which can each still show a stale state.
pub fn set_meeting_recording(app: &AppHandle, recording: bool) {
    change_meeting(app, move |meetings| meetings.set_recording(recording));
}

fn change_meeting(app: &AppHandle, change: impl FnOnce(&MeetingManager) + Send + 'static) {
    let Some(meetings) = app.try_state::<Arc<MeetingManager>>() else {
        warn!("MeetingManager not initialized");
        return;
    };
    let meetings = Arc::clone(&meetings);
    let app = app.clone();
    std::thread::spawn(move || {
        change(&meetings);
        // Also when nothing changed: the callers wait for this.
        crate::overlay::set_meeting_recording(&app, meetings.is_recording());
        // The Meetings page reads the list again.
        let _ = app.emit("meetings-changed", ());
    });
}

/// Listen for Unix signals that remotely toggle transcription.
///
/// SIGUSR2 toggles plain transcription on all Unix platforms. SIGUSR1
/// (transcription with post-processing) is only handled on macOS: on Linux,
/// WebKitGTK's JavaScriptCore garbage collector sends SIGUSR1 to its own
/// threads to suspend them, so handling it caused phantom recordings on every
/// GC cycle (#1660). Linux users should use `handy --toggle-post-process`
/// instead. SIGTERM saves a meeting being recorded, then quits.
#[cfg(unix)]
pub fn setup_signal_handler(app_handle: AppHandle) {
    #[cfg(target_os = "macos")]
    let mut signals = Signals::new([SIGUSR1, SIGUSR2, SIGTERM])
        .expect("failed to register transcription signal handlers");
    #[cfg(not(target_os = "macos"))]
    let mut signals =
        Signals::new([SIGUSR2, SIGTERM]).expect("failed to register transcription signal handlers");
    #[cfg(target_os = "macos")]
    debug!("Signal handlers registered (SIGUSR1, SIGUSR2)");
    #[cfg(not(target_os = "macos"))]
    debug!("Signal handler registered (SIGUSR2; SIGUSR1 is left to WebKitGTK)");
    thread::spawn(move || {
        for sig in signals.forever() {
            let (binding_id, signal_name) = match sig {
                #[cfg(target_os = "macos")]
                SIGUSR1 => ("transcribe_with_post_process", "SIGUSR1"),
                SIGUSR2 => ("transcribe", "SIGUSR2"),
                SIGTERM => {
                    // A meeting recording now is kept for the next start.
                    debug!("Received SIGTERM");
                    crate::finish_meeting_for_exit(&app_handle);
                    app_handle.exit(0);
                    // Still quit if the event loop never gets to it.
                    thread::sleep(std::time::Duration::from_secs(3));
                    std::process::exit(0);
                }
                _ => continue,
            };
            debug!("Received {signal_name}");
            send_transcription_input(&app_handle, binding_id, signal_name);
        }
    });
}
