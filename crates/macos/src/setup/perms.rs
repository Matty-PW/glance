//! Where the permissions page gets its states: the real ones, or (for
//! `--setup-demo` and snapshots) a script that touches nothing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glance_orchestrator::setup::{
    Access, PermissionAction, SetupBackend, Step, classify_mic, classify_screen,
};

use crate::permission::{
    MIC_SETTINGS, SCREEN_SETTINGS, mic_status, open_settings_pane, request_microphone,
    request_screen_recording, screen_recording_fresh, screen_recording_granted,
};
use crate::signing::Signing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Which {
    Screen,
    Mic,
}

pub(crate) trait Permissions {
    /// `asked`: the Screen Recording prompt was shown before.
    fn screen(&mut self, asked: bool) -> Access;
    fn mic(&mut self) -> Access;
    fn act(&mut self, which: Which, action: PermissionAction, backend: &dyn SetupBackend);
    fn signing(&mut self) -> Signing;
}

/// The real states. The fresh-process probe runs at most every 2 s, on a
/// helper thread, and only while this process says "not granted".
pub(crate) struct Real {
    fresh: Arc<Mutex<Option<bool>>>,
    probing: Arc<AtomicBool>,
    last_probe: Option<Instant>,
    signing: Option<Signing>,
}

impl Real {
    pub(crate) fn new() -> Self {
        Self {
            fresh: Arc::new(Mutex::new(None)),
            probing: Arc::new(AtomicBool::new(false)),
            last_probe: None,
            signing: None,
        }
    }
}

impl Permissions for Real {
    fn screen(&mut self, asked: bool) -> Access {
        let granted = screen_recording_granted();
        if !granted
            && !self.probing.load(Ordering::Relaxed)
            && self
                .last_probe
                .is_none_or(|t| t.elapsed() > Duration::from_secs(2))
        {
            self.last_probe = Some(Instant::now());
            self.probing.store(true, Ordering::Relaxed);
            let (fresh, probing) = (Arc::clone(&self.fresh), Arc::clone(&self.probing));
            std::thread::spawn(move || {
                let seen = screen_recording_fresh();
                *fresh.lock().unwrap_or_else(|p| p.into_inner()) = seen;
                probing.store(false, Ordering::Relaxed);
            });
        }
        let fresh = *self.fresh.lock().unwrap_or_else(|p| p.into_inner());
        classify_screen(granted, fresh, asked)
    }

    fn mic(&mut self) -> Access {
        classify_mic(mic_status())
    }

    fn act(&mut self, which: Which, action: PermissionAction, backend: &dyn SetupBackend) {
        match (which, action) {
            (Which::Screen, PermissionAction::Ask) => {
                backend.note_screen_asked();
                request_screen_recording();
            }
            (Which::Mic, PermissionAction::Ask) => request_microphone(|_| {}),
            (Which::Screen, PermissionAction::OpenSettings) => open_settings_pane(SCREEN_SETTINGS),
            (Which::Mic, PermissionAction::OpenSettings) => open_settings_pane(MIC_SETTINGS),
            (_, PermissionAction::Restart) => backend.restart(Step::Permissions),
        }
    }

    fn signing(&mut self) -> Signing {
        *self.signing.get_or_insert_with(crate::signing::signing)
    }
}

/// `--setup-demo`: Screen Recording starts off, "Open System Settings"
/// turns it on two seconds later but needs a restart, "Restart glance"
/// grants it; the microphone is granted a second after "Allow". The build
/// shows as ad hoc so the signing note appears.
pub(crate) struct Demo {
    screen: Access,
    mic: Access,
    screen_on_at: Option<Instant>,
    mic_on_at: Option<Instant>,
    signing: Signing,
}

impl Demo {
    pub(crate) fn new() -> Self {
        Self {
            screen: Access::Denied,
            mic: Access::NotAsked,
            screen_on_at: None,
            mic_on_at: None,
            signing: Signing::AdHoc,
        }
    }

    /// Fixed states for snapshots.
    pub(crate) fn fixed(screen: Access, mic: Access, signing: Signing) -> Self {
        Self {
            screen,
            mic,
            screen_on_at: None,
            mic_on_at: None,
            signing,
        }
    }
}

impl Permissions for Demo {
    fn screen(&mut self, _asked: bool) -> Access {
        if self.screen_on_at.is_some_and(|t| Instant::now() >= t) {
            self.screen_on_at = None;
            self.screen = Access::NeedsRestart;
        }
        self.screen
    }

    fn mic(&mut self) -> Access {
        if self.mic_on_at.is_some_and(|t| Instant::now() >= t) {
            self.mic_on_at = None;
            self.mic = Access::Granted;
        }
        self.mic
    }

    fn act(&mut self, which: Which, action: PermissionAction, backend: &dyn SetupBackend) {
        let soon = |s| Some(Instant::now() + Duration::from_secs(s));
        match (which, action) {
            (Which::Screen, PermissionAction::Restart) => {
                // The demo backend does not restart; the grant takes.
                backend.restart(Step::Permissions);
                self.screen = Access::Granted;
            }
            (Which::Screen, _) => self.screen_on_at = soon(2),
            (Which::Mic, _) => self.mic_on_at = soon(1),
        }
    }

    fn signing(&mut self) -> Signing {
        self.signing
    }
}
