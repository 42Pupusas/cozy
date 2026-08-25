//! [`MountService`] -- runs a mount on a worker thread and carries its
//! password prompt back to the event loop.
//!
//! # The problem this exists to solve
//!
//! `PasswordPrompt::ask` **blocks**. It is called from inside the mount, and
//! it must not return until the user has typed a reply. That is the only
//! shape polkit's conversation allows: `BeginAuthentication` does not return
//! until the answer is known.
//!
//! The event loop is the opposite. It must keep running to *draw* the prompt
//! the user is meant to type into, and to deliver their keystrokes.
//!
//! Calling `mount` on the UI thread therefore deadlocks in the most complete
//! way available: the prompt blocks waiting for input from a loop that is
//! blocked inside the prompt. The window would freeze -- undrawn, unable to
//! process the very keys it is waiting for.
//!
//! So the mount runs on a worker thread, and the prompt becomes a pair of
//! channels. [`ChannelPrompt::ask`] sends the question to the UI thread and
//! blocks on the reply -- blocking the *worker*, which is allowed and
//! correct, while the UI thread stays free to paint and collect keystrokes.
//!
//! ```text
//!   UI thread                         worker thread
//!   ---------                         -------------
//!   start() ------------------------> backend.mount(request)
//!   poll() -> Prompt {..}  <--------- ChannelPrompt::ask   (blocks)
//!   ...paints, collects keys...
//!   answer(Some(secret)) ------------> returns the reply
//!   poll() -> Finished(..) <--------- the outcome
//! ```
//!
//! # Why a dropped channel means dismissal
//!
//! The worker blocks on a reply that may never come: the user may close the
//! window with the prompt open. If that were a hang, the browser could not
//! exit -- and it would still hold a registered polkit agent.
//!
//! So the reply is delivered over a channel whose *sender* the UI thread
//! owns, and a receive error is read as dismissal. Dropping the service
//! releases the worker rather than waiting for it. This is the same reason
//! `PasswordPrompt` returns `Option<String>` at all.
//!
//! # Why the waker is signalled
//!
//! The loop sleeps until an event arrives. A prompt produced by a worker
//! thread is not a Wayland event, so nothing would wake the loop and the
//! modal would appear only when the user happened to move the mouse. Every
//! message therefore signals the [`waker::Waker`] the loop polls on.
//!
//! # Generic over the backend, deliberately
//!
//! The real backend needs D-Bus, systemd, polkit and a physical USB stick.
//! This type is generic so the tests below drive it with a fake, and prove
//! the *threading* -- the part that deadlocks, hangs, or drops a reply --
//! without any of that.

use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use volumes::mount_backend::{MountBackend, MountOutcome};
use volumes::mount_request::MountRequest;

/// Something the mount wants the UI to do or know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountProgress {
    /// The conversation is asking for a secret. The UI must open the modal
    /// and eventually call [`MountService::answer`], or the worker waits.
    Prompt {
        /// polkit's own wording, to be shown verbatim.
        prompt: String,
        /// Whether the reply may be echoed on screen.
        echo: bool,
    },
    /// The helper said something worth showing, e.g. "2 attempts remaining".
    Notice(String),
    /// The mount finished, one way or another.
    Finished(MountOutcome),
}

/// The prompt handed to the backend: a pair of channel ends pretending to be
/// a user.
///
/// Lives on the **worker** thread. `ask` blocks it, which is the entire
/// point -- the UI thread must stay free.
pub struct ChannelPrompt {
    /// Questions out to the UI thread.
    progress: Sender<MountProgress>,
    /// Replies back from it.
    replies: Receiver<Option<String>>,
    /// Woken so the loop notices a question immediately.
    wake: waker::Waker,
}

impl ChannelPrompt {
    /// Send one message and wake the loop to see it.
    fn report(&self, progress: MountProgress) -> bool {
        let sent = self.progress.send(progress).is_ok();
        self.wake.signal();
        sent
    }
}

impl mounter::PasswordPrompt for ChannelPrompt {
    /// Ask the UI thread, and block this one until it answers.
    ///
    /// A closed channel -- the window went away -- reads as dismissal, so the
    /// worker unblocks and the mount ends instead of hanging forever.
    fn ask(&mut self, prompt: &str, echo: bool) -> Option<String> {
        if !self.report(MountProgress::Prompt {
            prompt: prompt.to_owned(),
            echo,
        }) {
            return None;
        }
        // A closed channel means the window went away; `unwrap_or_default`
        // is `None`, which is dismissal.
        self.replies.recv().unwrap_or_default()
    }

    fn notify(&mut self, message: &str) {
        self.report(MountProgress::Notice(message.to_owned()));
    }
}

/// A mount in flight on a worker thread.
pub struct MountService {
    /// Messages from the worker.
    progress: Receiver<MountProgress>,
    /// Replies to the worker. Dropped to release a blocked prompt.
    replies: Option<Sender<Option<String>>>,
    /// Kept to detect completion; never joined on the UI thread.
    worker: Option<std::thread::JoinHandle<()>>,
}

impl MountService {
    /// Begin mounting `request`, building the backend from a prompt.
    ///
    /// The backend is built **on the worker thread**, from the closure: a
    /// D-Bus connection belongs to the thread that will use it, and building
    /// it here would put the connection's setup -- which can block -- on the
    /// UI thread.
    pub fn start<B, F>(request: &MountRequest, wake: &waker::Waker, build: F) -> Self
    where
        B: MountBackend,
        F: FnOnce(ChannelPrompt) -> B + Send + 'static,
    {
        let (progress_tx, progress_rx) = std::sync::mpsc::channel();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        let prompt = ChannelPrompt {
            progress: progress_tx.clone(),
            replies: reply_rx,
            wake: wake.clone(),
        };
        let job = request.clone();
        let wake_at_end = wake.clone();
        let worker = std::thread::spawn(move || {
            let mut backend = build(prompt);
            let outcome = backend.mount(&job);
            let _ = progress_tx.send(MountProgress::Finished(outcome));
            wake_at_end.signal();
        });
        Self {
            progress: progress_rx,
            replies: Some(reply_tx),
            worker: Some(worker),
        }
    }

    /// Take the next message, if the worker has produced one.
    ///
    /// Never blocks: the event loop calls this every pass and must return to
    /// painting whether or not anything arrived. A disconnected channel with
    /// nothing queued means the worker died without reporting -- a panic in
    /// the backend -- which is reported as a failure rather than leaving the
    /// UI waiting for a message that cannot come.
    pub fn poll(&mut self) -> Option<MountProgress> {
        match self.progress.try_recv() {
            Ok(progress) => Some(progress),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => self.worker.take().map(|_| {
                MountProgress::Finished(MountOutcome::Failed(String::from(
                    "the mount ended without reporting",
                )))
            }),
        }
    }

    /// Deliver the user's reply, releasing the blocked worker.
    ///
    /// `None` dismisses. Returns whether anyone was still listening.
    pub fn answer(&self, reply: Option<String>) -> bool {
        self.replies
            .as_ref()
            .is_some_and(|tx| tx.send(reply).is_ok())
    }

}

/// Releases a worker blocked on a prompt that will never be answered.
///
/// Dropping the reply sender turns the worker's `recv` into an error, which
/// `ChannelPrompt::ask` reads as a dismissal. Without this the browser could
/// not close with a prompt open.
impl Drop for MountService {
    fn drop(&mut self) {
        self.replies = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{ChannelPrompt, MountProgress, MountService};
    use mounter::PasswordPrompt;
    use volumes::mount_backend::{MountBackend, MountOutcome};
    use volumes::mount_point::MountPoint;
    use volumes::mount_request::MountRequest;

    /// A backend that asks for a password, then reports what it received.
    ///
    /// `ScriptedBackend` never prompts, so it cannot exercise the blocking
    /// path -- which is the only part of this module that can deadlock.
    struct AskingBackend {
        prompt: ChannelPrompt,
        /// How many times to ask before answering.
        rounds: usize,
        /// What to report when the replies are in.
        outcome: MountOutcome,
    }

    impl MountBackend for AskingBackend {
        fn mount(&mut self, _request: &MountRequest) -> MountOutcome {
            for _ in 0..self.rounds {
                let Some(reply) = self.prompt.ask("Password: ", false) else {
                    return MountOutcome::Cancelled;
                };
                if reply != "correct" {
                    return MountOutcome::Denied;
                }
            }
            self.outcome.clone()
        }

        fn is_available(&self) -> bool {
            true
        }
    }

    fn request() -> MountRequest {
        MountRequest::new(
            String::from("/dev/sdb1"),
            String::from("vfat"),
            MountPoint::for_user("user", Some("KINGSTON"), "/dev/sdb1")
                .expect("a legal mount point"),
            1000,
            1000,
        )
    }

    /// Wait for a message, failing rather than hanging the suite forever.
    fn next(service: &mut MountService) -> MountProgress {
        for _ in 0..2000 {
            if let Some(progress) = service.poll() {
                return progress;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the worker produced no message");
    }

    #[test]
    fn a_mount_that_needs_no_password_reports_only_its_outcome() {
        let wake = waker::Waker::new().expect("an eventfd");
        let mut service = MountService::start(&request(), &wake, |_prompt| {
            volumes::ScriptedBackend::always_succeeding()
        });
        assert_eq!(
            next(&mut service),
            MountProgress::Finished(MountOutcome::Mounted)
        );
    }

    /// The whole reason this module exists: the question must reach the UI
    /// thread while the worker waits, rather than blocking the loop.
    #[test]
    fn a_prompt_reaches_the_ui_thread_and_the_reply_reaches_the_worker() {
        let wake = waker::Waker::new().expect("an eventfd");
        let mut service = MountService::start(&request(), &wake, |prompt| AskingBackend {
            prompt,
            rounds: 1,
            outcome: MountOutcome::Mounted,
        });
        assert_eq!(
            next(&mut service),
            MountProgress::Prompt {
                prompt: String::from("Password: "),
                echo: false,
            }
        );
        assert!(service.answer(Some(String::from("correct"))));
        assert_eq!(
            next(&mut service),
            MountProgress::Finished(MountOutcome::Mounted)
        );
    }

    /// The loop sleeps until something wakes it. A prompt is not a Wayland
    /// event, so without this signal the modal appears only when the user
    /// happens to move the mouse.
    #[test]
    fn a_prompt_wakes_the_event_loop() {
        let wake = waker::Waker::new().expect("an eventfd");
        wake.drain();
        let mut service = MountService::start(&request(), &wake, |prompt| AskingBackend {
            prompt,
            rounds: 1,
            outcome: MountOutcome::Mounted,
        });
        assert!(
            wake.wait(2000),
            "the loop must be woken to paint the prompt"
        );
        let _ = next(&mut service);
        service.answer(None);
    }

    /// A conversation may ask more than once -- a mistyped password is the
    /// normal case, not an exceptional one.
    #[test]
    fn several_prompts_are_answered_in_sequence() {
        let wake = waker::Waker::new().expect("an eventfd");
        let mut service = MountService::start(&request(), &wake, |prompt| AskingBackend {
            prompt,
            rounds: 3,
            outcome: MountOutcome::Mounted,
        });
        for _ in 0..3 {
            assert!(matches!(
                next(&mut service),
                MountProgress::Prompt { .. }
            ));
            assert!(service.answer(Some(String::from("correct"))));
        }
        assert_eq!(
            next(&mut service),
            MountProgress::Finished(MountOutcome::Mounted)
        );
    }

    /// Dismissal is a normal outcome and must unblock the worker.
    #[test]
    fn dismissing_the_prompt_ends_the_mount_as_cancelled() {
        let wake = waker::Waker::new().expect("an eventfd");
        let mut service = MountService::start(&request(), &wake, |prompt| AskingBackend {
            prompt,
            rounds: 1,
            outcome: MountOutcome::Mounted,
        });
        assert!(matches!(next(&mut service), MountProgress::Prompt { .. }));
        assert!(service.answer(None));
        assert_eq!(
            next(&mut service),
            MountProgress::Finished(MountOutcome::Cancelled)
        );
    }

    /// Closing the window with a prompt open must not hang the browser. The
    /// dropped sender is what releases the worker.
    #[test]
    fn dropping_the_service_releases_a_worker_blocked_on_a_prompt() {
        let wake = waker::Waker::new().expect("an eventfd");
        let mut service = MountService::start(&request(), &wake, |prompt| AskingBackend {
            prompt,
            rounds: 1,
            outcome: MountOutcome::Mounted,
        });
        assert!(matches!(next(&mut service), MountProgress::Prompt { .. }));
        let worker = service.worker.take().expect("the worker is running");
        drop(service);
        for _ in 0..2000 {
            if worker.is_finished() {
                worker.join().expect("the worker ends cleanly");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the worker is still blocked on a prompt nobody will answer");
    }

    /// A helper notice arrives between prompts and is worthless afterwards,
    /// so it must be delivered as its own message rather than batched.
    #[test]
    fn a_notice_reaches_the_ui_as_its_own_message() {
        let wake = waker::Waker::new().expect("an eventfd");
        let (progress, rx) = std::sync::mpsc::channel();
        let (_reply_tx, replies) = std::sync::mpsc::channel();
        let mut prompt = ChannelPrompt {
            progress,
            replies,
            wake,
        };
        prompt.notify("2 attempts remaining");
        assert_eq!(
            rx.try_recv(),
            Ok(MountProgress::Notice(String::from("2 attempts remaining")))
        );
    }

    /// A prompt asked after the window has gone must not block the worker
    /// forever waiting for a reply that cannot arrive.
    #[test]
    fn a_prompt_with_no_listener_dismisses_rather_than_blocking() {
        let wake = waker::Waker::new().expect("an eventfd");
        let (progress, dropped) = std::sync::mpsc::channel();
        let (_reply_tx, replies) = std::sync::mpsc::channel();
        drop(dropped);
        let mut prompt = ChannelPrompt {
            progress,
            replies,
            wake,
        };
        assert_eq!(prompt.ask("Password: ", false), None);
    }
}
