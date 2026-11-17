//! The microphone and the speech recognizer each run on a dedicated OS
//! thread. Both are blocking (opening a CoreAudio stream takes ~100 ms;
//! transcription is CPU-heavy), and one thread per device keeps commands in
//! the order they were sent: a `stop` can never overtake its `start`.

use std::sync::mpsc;
use std::thread;

use glance_audio::{AudioClip, AudioError, AudioInput, Recording, resample};
use glance_stt::{SpeechToText, SttError, Transcript};
use tokio::sync::oneshot;

/// A worker's answer. Resolves to `Err` if the worker thread is gone.
pub(crate) type Reply<T> = oneshot::Receiver<T>;

enum MicCommand {
    Start(oneshot::Sender<Result<(), AudioError>>),
    /// Stop and resample the recording to the speech model's rate.
    Stop(oneshot::Sender<Result<Recording, AudioError>>),
}

pub(crate) struct MicWorker {
    commands: mpsc::Sender<MicCommand>,
}

impl MicWorker {
    pub(crate) fn spawn(mut mic: Box<dyn AudioInput>) -> Self {
        let (commands, inbox) = mpsc::channel();
        thread::Builder::new()
            .name("glance-mic".into())
            .spawn(move || {
                while let Ok(command) = inbox.recv() {
                    match command {
                        MicCommand::Start(reply) => {
                            let _ = reply.send(mic.start());
                        }
                        MicCommand::Stop(reply) => {
                            let result = mic.stop().and_then(|mut recording| {
                                recording.clip =
                                    resample(&recording.clip, glance_stt::SAMPLE_RATE)?;
                                Ok(recording)
                            });
                            let _ = reply.send(result);
                        }
                    }
                }
            })
            .expect("could not start the microphone thread");
        Self { commands }
    }

    pub(crate) fn start(&self) -> Reply<Result<(), AudioError>> {
        let (reply, answer) = oneshot::channel();
        // If the thread is gone the reply is dropped and `answer` errors.
        let _ = self.commands.send(MicCommand::Start(reply));
        answer
    }

    pub(crate) fn stop(&self) -> Reply<Result<Recording, AudioError>> {
        let (reply, answer) = oneshot::channel();
        let _ = self.commands.send(MicCommand::Stop(reply));
        answer
    }
}

struct SttJob {
    clip: AudioClip,
    reply: oneshot::Sender<Result<Transcript, SttError>>,
}

pub(crate) struct SttWorker {
    jobs: mpsc::Sender<SttJob>,
}

impl SttWorker {
    pub(crate) fn spawn(mut stt: Box<dyn SpeechToText>) -> Self {
        let (jobs, inbox) = mpsc::channel::<SttJob>();
        thread::Builder::new()
            .name("glance-stt".into())
            .spawn(move || {
                while let Ok(job) = inbox.recv() {
                    // A cancelled turn has dropped its receiver; the result
                    // is simply discarded.
                    if job.reply.is_closed() {
                        continue;
                    }
                    let _ = job.reply.send(stt.transcribe(&job.clip));
                }
            })
            .expect("could not start the speech recognition thread");
        Self { jobs }
    }

    pub(crate) fn transcribe(&self, clip: AudioClip) -> Reply<Result<Transcript, SttError>> {
        let (reply, answer) = oneshot::channel();
        let _ = self.jobs.send(SttJob { clip, reply });
        answer
    }
}
