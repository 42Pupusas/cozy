//! [`IdleWait`] -- where the event loop sleeps between things to do.
//!
//! Every source of work is a descriptor, so the loop blocks in `poll(2)` with
//! no timeout and wakes only for one of them:
//!
//! * the toplevel's eventfd, signalled by the Wayland reader and by the
//!   mount, folder-size and activation workers;
//! * the open directory's inotify fd;
//! * the mount table (`POLLPRI`) and udev's block records, for devices.
//!
//! The one deadline is key auto-repeat, which is a real due time rather than
//! a guess at how long to sleep.

use platform::toplevel::Toplevel;
use waker::{Interest, PollSet};

use crate::window::FilesWindow;

/// Which device sources were ready when the loop woke.
#[derive(Clone, Copy)]
pub struct Wakeup {
    pub mounts: bool,
    pub udev: bool,
}

/// Builds the descriptor set for one sleep and reports what fired.
pub struct IdleWait;

impl IdleWait {
    /// Sleep until a source has work, handing device readiness to `painter`.
    pub fn sleep(window: &Toplevel, painter: &mut FilesWindow) {
        let mut set = PollSet::new();
        let waker = set.watch(window.waker().raw_fd(), Interest::Readable);
        if let Some(fd) = painter.watch_fd() {
            set.watch(fd, Interest::Readable);
        }
        let mounts = painter
            .mounts_fd()
            .map(|fd| set.watch(fd, Interest::Priority));
        let udev = painter
            .udev_fd()
            .map(|fd| set.watch(fd, Interest::Readable));

        let _ = set.wait(window.repeat_wait_ms());

        if set.is_ready(waker) {
            window.waker().drain();
        }
        painter.absorb_device_signals(Wakeup {
            mounts: mounts.is_some_and(|i| set.is_ready(i)),
            udev: udev.is_some_and(|i| set.is_ready(i)),
        });
    }
}
