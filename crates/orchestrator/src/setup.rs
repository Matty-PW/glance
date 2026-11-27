//! The first-launch setup window, as plain data: its steps and the rules
//! for moving between them ([`Flow`]), when it opens ([`launch_decision`]),
//! how permission states read ([`Access`]), which stage a failed test turn
//! points at ([`stage_of`]), and the [`SetupBackend`] the app implements.
//! The window itself is native, in the platform crate, and sees only these
//! types.
//!
//! Rules (maintainer, 2026-11-27): nothing here blocks quitting; skipping
//! never marks setup complete while required models are missing; config is
//! written by the app through the strict parser only once the chosen
//! models are verified.

use std::time::Duration;

use crate::TurnError;
use crate::settings::{DownloadIssue, DownloadProgress};

/// A page of the setup window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Welcome,
    YourMac,
    Permissions,
    Download,
    TryIt,
    Done,
    /// Everything was already in place: one short page.
    AllSet,
}

impl Step {
    /// The steps of a full run, in order (the progress dots).
    pub const FLOW: [Step; 6] = [
        Step::Welcome,
        Step::YourMac,
        Step::Permissions,
        Step::Download,
        Step::TryIt,
        Step::Done,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Step::Welcome => "welcome",
            Step::YourMac => "your_mac",
            Step::Permissions => "permissions",
            Step::Download => "download",
            Step::TryIt => "try_it",
            Step::Done => "done",
            Step::AllSet => "all_set",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::FLOW
            .into_iter()
            .chain([Step::AllSet])
            .find(|s| s.as_str() == text)
    }

    /// Position in [`FLOW`](Self::FLOW), 0-based; `None` for `AllSet`.
    pub fn index(self) -> Option<usize> {
        Self::FLOW.iter().position(|s| *s == self)
    }

    pub fn title(self) -> &'static str {
        match self {
            Step::Welcome => "Welcome to glance",
            Step::YourMac => "Your Mac",
            Step::Permissions => "Permissions",
            Step::Download => "Download models",
            Step::TryIt => "Try it",
            Step::Done => "Done",
            Step::AllSet => "You're all set",
        }
    }
}

/// What the flow needs to know about the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Facts {
    /// Every model the chosen setup needs is on disk with its pinned size
    /// (and, after a download in this session, checked against its pins).
    pub models_ready: bool,
}

/// What skipping does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// The models are there: setup counts as done.
    MarkComplete,
    /// The models are missing: close the window; setup opens again at the
    /// next launch.
    CloseOnly,
}

/// The step state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flow {
    step: Step,
}

impl Flow {
    pub fn new(start: Step) -> Self {
        Self { step: start }
    }

    pub fn step(&self) -> Step {
        self.step
    }

    /// "Step 2 of 6": (1-based position, count); `None` on the all-set page.
    pub fn position(&self) -> Option<(usize, usize)> {
        self.step.index().map(|i| (i + 1, Step::FLOW.len()))
    }

    pub fn can_back(&self) -> bool {
        !matches!(self.step, Step::Welcome | Step::AllSet)
    }

    /// Next is offered only where there is a next page, and from Download
    /// only once the models are ready.
    pub fn can_next(&self, facts: Facts) -> bool {
        match self.step {
            Step::Download => facts.models_ready,
            Step::Done | Step::AllSet => false,
            _ => true,
        }
    }

    pub fn next(&mut self, facts: Facts) -> Option<Step> {
        if !self.can_next(facts) {
            return None;
        }
        let i = self.step.index()?;
        self.step = *Step::FLOW.get(i + 1)?;
        Some(self.step)
    }

    pub fn back(&mut self) -> Option<Step> {
        if !self.can_back() {
            return None;
        }
        let i = self.step.index()?;
        self.step = Step::FLOW[i.checked_sub(1)?];
        Some(self.step)
    }

    /// Jump straight to a page (the dots, or a "Fix" link).
    pub fn go(&mut self, step: Step) {
        self.step = step;
    }

    /// "Start using glance" is allowed only with the models in place.
    pub fn can_finish(&self, facts: Facts) -> bool {
        facts.models_ready
    }

    pub fn skip(&self, facts: Facts) -> Skip {
        if facts.models_ready {
            Skip::MarkComplete
        } else {
            Skip::CloseOnly
        }
    }
}

/// What a launch knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchFacts {
    /// The completed-setup marker is there.
    pub completed: bool,
    pub screen: Access,
    pub mic: Access,
    /// The configured models are on disk.
    pub models_ready: bool,
    /// The page the last run of setup was on (kept across a restart).
    pub saved_step: Option<Step>,
}

/// Whether the setup window opens at launch, and where.
pub fn launch_decision(f: LaunchFacts) -> Option<Step> {
    let all_ok = f.screen.is_ok() && f.mic.is_ok() && f.models_ready;
    match (f.completed, all_ok) {
        (true, true) => None,
        (false, true) => Some(Step::AllSet),
        // Setup was under way (or restarted for a permission): carry on.
        (false, false) => Some(f.saved_step.unwrap_or(Step::Welcome)),
        // Done before, and something went missing since: go to it.
        (true, false) if !f.screen.is_ok() || !f.mic.is_ok() => Some(Step::Permissions),
        (true, false) => Some(Step::Download),
    }
}

/// A permission's state, as the setup page shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Granted,
    /// Not asked yet: "Allow" shows the macOS prompt.
    NotAsked,
    /// Turned off (or the prompt was declined): only System Settings can
    /// change it.
    Denied,
    /// Blocked by a device policy (MDM, parental controls).
    Restricted,
    /// Allowed in System Settings, but this running glance cannot use it
    /// until it restarts (Screen Recording).
    NeedsRestart,
    Unknown,
}

/// What the button next to a permission does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionAction {
    /// Show the macOS prompt.
    Ask,
    OpenSettings,
    Restart,
}

impl Access {
    pub fn is_ok(self) -> bool {
        self == Access::Granted
    }

    /// The state in words (shown beside a symbol, so never colour alone).
    pub fn label(self) -> &'static str {
        match self {
            Access::Granted => "Allowed",
            Access::NotAsked => "Not allowed yet",
            Access::Denied => "Not allowed",
            Access::Restricted => "Blocked by a device policy",
            Access::NeedsRestart => "Allowed. Restart glance to use it",
            Access::Unknown => "Unknown",
        }
    }

    pub fn action(self) -> Option<PermissionAction> {
        match self {
            Access::Granted | Access::Restricted => None,
            Access::NotAsked => Some(PermissionAction::Ask),
            Access::Denied | Access::Unknown => Some(PermissionAction::OpenSettings),
            Access::NeedsRestart => Some(PermissionAction::Restart),
        }
    }
}

/// Screen Recording. macOS answers only yes or no for the running process,
/// and a grant made in System Settings usually reaches a running app only
/// after it restarts. So: `in_process` is this process's preflight,
/// `fresh` the same check made by a new child process (which sees the
/// current grant), `asked` whether glance has shown the prompt before.
pub fn classify_screen(in_process: bool, fresh: Option<bool>, asked: bool) -> Access {
    match (in_process, fresh) {
        (true, _) => Access::Granted,
        (false, Some(true)) => Access::NeedsRestart,
        (false, _) if asked => Access::Denied,
        (false, _) => Access::NotAsked,
    }
}

/// The microphone's status as AVFoundation reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicStatus {
    NotDetermined,
    Authorized,
    Denied,
    Restricted,
    Unknown,
}

pub fn classify_mic(status: MicStatus) -> Access {
    match status {
        MicStatus::NotDetermined => Access::NotAsked,
        MicStatus::Authorized => Access::Granted,
        MicStatus::Denied => Access::Denied,
        MicStatus::Restricted => Access::Restricted,
        MicStatus::Unknown => Access::Unknown,
    }
}

/// The part of the pipeline a failed test points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Capture,
    SpeechToText,
    Model,
    Voice,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Capture => "Capture",
            Stage::SpeechToText => "Speech-to-text",
            Stage::Model => "Model",
            Stage::Voice => "Voice",
        }
    }

    /// For log lines.
    pub fn code(self) -> &'static str {
        match self {
            Stage::Capture => "capture",
            Stage::SpeechToText => "stt",
            Stage::Model => "model",
            Stage::Voice => "voice",
        }
    }
}

/// Which stage failed, and what to do about it (fixed text).
pub fn stage_of(error: &TurnError) -> (Stage, &'static str) {
    match error {
        TurnError::ScreenPermission => (
            Stage::Capture,
            "glance may not capture the screen. Allow Screen Recording on the Permissions page.",
        ),
        TurnError::Capture(_) => (
            Stage::Capture,
            "The test image could not be prepared. Try again, and report it if it keeps failing.",
        ),
        TurnError::Mic(_) => (
            Stage::SpeechToText,
            "The microphone did not start. Check that one is connected and allowed on the \
             Permissions page.",
        ),
        TurnError::SilentMic => (
            Stage::SpeechToText,
            "The microphone sent only silence. Allow Microphone on the Permissions page.",
        ),
        TurnError::Stt(_) => (
            Stage::SpeechToText,
            "Speech recognition failed. Check the speech model in Settings, Models.",
        ),
        TurnError::NoSpeech => (
            Stage::SpeechToText,
            "No words were recognised. Speak a little closer to the microphone and try again.",
        ),
        TurnError::Chat(_) => (
            Stage::Model,
            "The model server did not answer. Check that llama-server was found on the Your \
             Mac page, then try again.",
        ),
        TurnError::EmptyReply | TurnError::BadSignal => {
            (Stage::Model, "The model gave no usable answer. Try again.")
        }
        TurnError::NoDeep | TurnError::NothingToReask | TurnError::DeepLoad(_) => (
            Stage::Model,
            "The bigger model is not available. The test uses the small one; try again.",
        ),
        TurnError::Speech(_) => (
            Stage::Voice,
            "The voice could not play. Check the sound output, or choose the macOS voice in \
             Settings, Voice.",
        ),
        TurnError::WorkerGone => (
            Stage::Model,
            "Part of glance stopped. Quit and start glance again.",
        ),
    }
}

/// The test question's state.
#[derive(Debug, Clone, PartialEq)]
pub enum TestResult {
    Idle,
    /// The models are still starting.
    Starting,
    /// Recording: the user reads the suggested question.
    Listening,
    /// Transcribing, thinking or speaking.
    Working,
    Works {
        first_word: Duration,
    },
    Failed {
        stage: Stage,
        hint: String,
    },
}

impl TestResult {
    pub fn is_running(&self) -> bool {
        matches!(
            self,
            TestResult::Starting | TestResult::Listening | TestResult::Working
        )
    }
}

/// "Works: first word in 1.2 s".
pub fn works_line(first_word: Duration) -> String {
    format!("Works: first word in {:.1} s", first_word.as_secs_f64())
}

/// The suggested test question (it matches the sample image).
pub const TEST_QUESTION: &str = "What does this chart show?";
/// The test records for at most this long.
pub const TEST_LISTEN: Duration = Duration::from_secs(8);

/// RAM tiers of DESIGN §7.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RamTier {
    /// Under 16 GB: not supported; only the smallest setup, with a warning.
    BelowMinimum,
    Gb16,
    Gb32,
    Gb48,
}

impl RamTier {
    /// From `hw.memsize`, in GiB as Apple counts them (16 GB = 2^34 bytes).
    pub fn of(ram_bytes: u64) -> Self {
        let gib = ram_bytes / (1 << 30);
        match gib {
            0..16 => RamTier::BelowMinimum,
            16..32 => RamTier::Gb16,
            32..48 => RamTier::Gb32,
            _ => RamTier::Gb48,
        }
    }

    /// Whether glance's numbers were measured on a Mac of this tier. Only
    /// the 48 GB reference machine has been measured.
    pub fn tested(self) -> bool {
        self == RamTier::Gb48
    }

    pub fn label(self) -> &'static str {
        match self {
            RamTier::BelowMinimum => "under 16 GB",
            RamTier::Gb16 => "16 GB class",
            RamTier::Gb32 => "32 GB class",
            RamTier::Gb48 => "48 GB or more",
        }
    }
}

/// The Mac, as the Your Mac page shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineInfo {
    /// "Apple M4 Pro".
    pub chip: Option<String>,
    pub ram_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    /// The llama-server glance will run, or why there is none.
    pub llama_server: Result<String, String>,
}

/// One setup choice.
#[derive(Debug, Clone, PartialEq)]
pub struct PresetInfo {
    pub id: String,
    pub name: String,
    /// One line: what it is for.
    pub summary: String,
    /// What it includes, one entry per model ("Vision: Qwen3-VL 4B").
    pub includes: Vec<String>,
    /// Bytes of all its models, and what is still to download.
    pub total_bytes: u64,
    pub missing_bytes: u64,
    /// Measured memory, e.g. "about 7.0 GB".
    pub memory: String,
    /// Where the memory figure comes from, or "untested on this tier".
    pub memory_note: String,
    /// Offered on this Mac.
    pub offered: bool,
    /// The free space it needs is there (missing bytes plus 2 GB).
    pub fits_disk: bool,
}

/// Everything the setup window shows.
#[derive(Debug, Clone, PartialEq)]
pub struct SetupSnapshot {
    pub machine: MachineInfo,
    pub tier: Option<RamTier>,
    /// Every preset; `offered` says which can be chosen here.
    pub presets: Vec<PresetInfo>,
    pub recommended: String,
    pub chosen: String,
    /// The chosen preset's models are present (and checked after a
    /// download).
    pub models_ready: bool,
    pub download: Option<DownloadProgress>,
    pub test: TestResult,
    /// talk, look, think chords ("Ctrl+Opt+G").
    pub hotkeys: [String; 3],
    /// "Kokoro (af_heart)" or "macOS voice".
    pub voice: String,
    /// glance must restart for the setup's choice to take effect (it was
    /// already running other models).
    pub needs_restart: bool,
    /// glance has shown the Screen Recording prompt before (macOS shows it
    /// once; after that only System Settings can change the answer).
    pub screen_asked: bool,
}

/// Implemented by the app.
pub trait SetupBackend: Send + Sync {
    fn snapshot(&self) -> SetupSnapshot;
    /// Choose a preset (remembered, not yet written to config.toml).
    fn choose(&self, preset: &str);
    /// Download what the chosen preset is missing.
    fn download(&self) -> Result<(), DownloadIssue>;
    fn pause(&self);
    fn resume(&self) -> Result<(), DownloadIssue>;
    fn cancel(&self);
    /// Start the test question: the next turn uses the sample image and
    /// records the user. Returns at once; [`stop_test`](Self::stop_test)
    /// ends the recording (or it ends by itself).
    fn start_test(&self) -> Result<(), String>;
    fn stop_test(&self);
    /// Remember the page (so a restart comes back to it).
    fn save_step(&self, step: Step);
    /// "Start using glance": write the choice (if not yet) and mark setup
    /// complete. Refused while models are missing.
    fn finish(&self) -> Result<(), String>;
    /// "Skip setup". Marks setup complete only when the models are there.
    fn skip(&self) -> Skip;
    /// Restart glance and come back to `step`.
    fn restart(&self, step: Step);
    /// The Screen Recording prompt was just shown.
    fn note_screen_asked(&self);
}

#[cfg(test)]
mod tests {
    use super::*;

    const READY: Facts = Facts { models_ready: true };
    const MISSING: Facts = Facts {
        models_ready: false,
    };

    #[test]
    fn the_flow_walks_forward_and_back() {
        let mut flow = Flow::new(Step::Welcome);
        assert!(!flow.can_back());
        assert_eq!(flow.position(), Some((1, 6)));
        assert_eq!(flow.next(MISSING), Some(Step::YourMac));
        assert_eq!(flow.next(MISSING), Some(Step::Permissions));
        assert_eq!(flow.next(MISSING), Some(Step::Download));
        // Download waits for the models.
        assert!(!flow.can_next(MISSING));
        assert_eq!(flow.next(MISSING), None);
        assert_eq!(flow.step(), Step::Download);
        assert_eq!(flow.next(READY), Some(Step::TryIt));
        assert_eq!(flow.next(READY), Some(Step::Done));
        assert_eq!(flow.position(), Some((6, 6)));
        assert_eq!(flow.next(READY), None);
        assert_eq!(flow.back(), Some(Step::TryIt));
        flow.go(Step::Welcome);
        assert_eq!(flow.back(), None);
    }

    #[test]
    fn finishing_and_skipping_need_the_models() {
        let flow = Flow::new(Step::Download);
        assert!(!flow.can_finish(MISSING));
        assert!(flow.can_finish(READY));
        assert_eq!(flow.skip(MISSING), Skip::CloseOnly);
        assert_eq!(flow.skip(READY), Skip::MarkComplete);
        let all_set = Flow::new(Step::AllSet);
        assert_eq!(all_set.position(), None);
        assert!(!all_set.can_back() && !all_set.can_next(READY));
    }

    #[test]
    fn steps_round_trip_through_their_names() {
        for step in Step::FLOW.into_iter().chain([Step::AllSet]) {
            assert_eq!(Step::parse(step.as_str()), Some(step));
        }
        assert_eq!(Step::parse("nope"), None);
    }

    fn facts(completed: bool, screen: Access, mic: Access, models: bool) -> LaunchFacts {
        LaunchFacts {
            completed,
            screen,
            mic,
            models_ready: models,
            saved_step: None,
        }
    }

    #[test]
    fn launch_decides_whether_and_where_setup_opens() {
        use Access::*;
        assert_eq!(launch_decision(facts(true, Granted, Granted, true)), None);
        // Everything in place but never set up: the short page.
        assert_eq!(
            launch_decision(facts(false, Granted, Granted, true)),
            Some(Step::AllSet)
        );
        assert_eq!(
            launch_decision(facts(false, NotAsked, NotAsked, false)),
            Some(Step::Welcome)
        );
        // A restart for Screen Recording comes back to Permissions.
        let restarted = LaunchFacts {
            saved_step: Some(Step::Permissions),
            ..facts(false, Granted, NotAsked, false)
        };
        assert_eq!(launch_decision(restarted), Some(Step::Permissions));
        // Done before; something went missing since.
        assert_eq!(
            launch_decision(facts(true, Denied, Granted, true)),
            Some(Step::Permissions)
        );
        assert_eq!(
            launch_decision(facts(true, Granted, NotAsked, true)),
            Some(Step::Permissions)
        );
        assert_eq!(
            launch_decision(facts(true, Granted, Granted, false)),
            Some(Step::Download)
        );
        // Granted but waiting for a restart is not "in place".
        assert_eq!(
            launch_decision(facts(true, NeedsRestart, Granted, true)),
            Some(Step::Permissions)
        );
    }

    #[test]
    fn screen_recording_states() {
        assert_eq!(classify_screen(true, None, false), Access::Granted);
        assert_eq!(classify_screen(true, Some(false), true), Access::Granted);
        assert_eq!(
            classify_screen(false, Some(true), true),
            Access::NeedsRestart
        );
        assert_eq!(
            classify_screen(false, Some(true), false),
            Access::NeedsRestart
        );
        assert_eq!(classify_screen(false, Some(false), false), Access::NotAsked);
        assert_eq!(classify_screen(false, None, false), Access::NotAsked);
        assert_eq!(classify_screen(false, Some(false), true), Access::Denied);
        assert_eq!(classify_screen(false, None, true), Access::Denied);
    }

    #[test]
    fn microphone_states_and_actions() {
        assert_eq!(classify_mic(MicStatus::Authorized), Access::Granted);
        assert_eq!(classify_mic(MicStatus::NotDetermined), Access::NotAsked);
        assert_eq!(classify_mic(MicStatus::Denied), Access::Denied);
        assert_eq!(classify_mic(MicStatus::Restricted), Access::Restricted);
        assert_eq!(classify_mic(MicStatus::Unknown), Access::Unknown);
        assert_eq!(Access::Granted.action(), None);
        assert_eq!(Access::Restricted.action(), None);
        assert_eq!(Access::NotAsked.action(), Some(PermissionAction::Ask));
        assert_eq!(
            Access::Denied.action(),
            Some(PermissionAction::OpenSettings)
        );
        assert_eq!(
            Access::NeedsRestart.action(),
            Some(PermissionAction::Restart)
        );
        assert!(!Access::NeedsRestart.is_ok());
    }

    #[test]
    fn every_turn_error_names_a_stage() {
        use TurnError::*;
        let cases = [
            (ScreenPermission, Stage::Capture),
            (Capture("x".into()), Stage::Capture),
            (Mic("x".into()), Stage::SpeechToText),
            (SilentMic, Stage::SpeechToText),
            (Stt("x".into()), Stage::SpeechToText),
            (NoSpeech, Stage::SpeechToText),
            (Chat("x".into()), Stage::Model),
            (EmptyReply, Stage::Model),
            (BadSignal, Stage::Model),
            (NoDeep, Stage::Model),
            (NothingToReask, Stage::Model),
            (DeepLoad("x".into()), Stage::Model),
            (Speech("x".into()), Stage::Voice),
            (WorkerGone, Stage::Model),
        ];
        for (error, stage) in cases {
            let (got, hint) = stage_of(&error);
            assert_eq!(got, stage, "{error}");
            assert!(hint.ends_with('.') && !hint.contains("x"), "{hint}");
        }
        assert_eq!(
            works_line(Duration::from_millis(1234)),
            "Works: first word in 1.2 s"
        );
    }

    #[test]
    fn ram_tiers_follow_design_7_4() {
        let gib = |n: u64| n << 30;
        assert_eq!(RamTier::of(gib(8)), RamTier::BelowMinimum);
        assert_eq!(RamTier::of(gib(16) - 1), RamTier::BelowMinimum);
        assert_eq!(RamTier::of(gib(16)), RamTier::Gb16);
        assert_eq!(RamTier::of(gib(24)), RamTier::Gb16);
        assert_eq!(RamTier::of(gib(32)), RamTier::Gb32);
        assert_eq!(RamTier::of(gib(36)), RamTier::Gb32);
        assert_eq!(RamTier::of(gib(48)), RamTier::Gb48);
        assert_eq!(RamTier::of(gib(128)), RamTier::Gb48);
        assert!(RamTier::Gb48.tested());
        assert!(!RamTier::Gb16.tested() && !RamTier::Gb32.tested());
    }
}
