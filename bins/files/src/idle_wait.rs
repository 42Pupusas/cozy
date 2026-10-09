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
//! The deadlines are key auto-repeat, which is a real due time rather than
//! a guess at how long to sleep, and the next frame of a tile glide while
//! one is running. The compositor's frame callback wakes the loop through the
//! waker as well, so the glide deadline is only the fallback for a callback
//! that never comes.

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
    /// The sooner of two deadlines, where `-1` means none.
    const fn timeout(repeat_ms: i32, motion_ms: Option<i32>) -> i32 {
        match (repeat_ms, motion_ms) {
            (repeat, Some(motion)) if repeat < 0 || motion < repeat => motion,
            (repeat, _) => repeat,
        }
    }

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

        let _ = set.wait(Self::timeout(
            window.repeat_wait_ms(),
            painter.motion_wait_ms(),
        ));

        if set.is_ready(waker) {
            window.waker().drain();
        }
        painter.absorb_device_signals(Wakeup {
            mounts: mounts.is_some_and(|i| set.is_ready(i)),
            udev: udev.is_some_and(|i| set.is_ready(i)),
        });
    }
}
