//! Blocks until the session bus socket file is created.
//!
//! The tray reconnects after the bus goes away. Whether the bus is back is
//! a filesystem fact: its socket file is created again. Watching the
//! containing directory turns that fact into a wakeup, so the tray thread
//! sleeps in `poll(2)` until the moment it can succeed.

use std::ffi::{CString, OsString};
use std::path::Path;

use dbus::SessionAddress;
use inotify_events::InotifyEvents;
use waker::Poller;

pub struct BusSocketWatch {
    inotify: ququmatz::Inotify,
    socket_name: OsString,
}

impl BusSocketWatch {
    const Q_OVERFLOW: u32 = 0x0000_4000;
    const BLOCK_FOREVER: i32 = -1;

    /// Watch for the socket file the current environment points at.
    ///
    /// # Errors
    /// Describes why no watch could be armed: an abstract address, no
    /// runtime directory, or an inotify failure.
    pub fn arm() -> Result<Self, String> {
        let socket = SessionAddress::expected_socket_path()
            .ok_or_else(|| "the session bus address names no socket file".to_owned())?;
        Self::arm_on(&socket)
    }

    /// Watch for `socket` to be created.
    ///
    /// # Errors
    /// Describes why the containing directory could not be watched.
    pub fn arm_on(socket: &Path) -> Result<Self, String> {
        let directory = socket
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", socket.display()))?;
        let socket_name = socket
            .file_name()
            .ok_or_else(|| format!("{} has no file name", socket.display()))?
            .to_owned();
        let target = CString::new(directory.as_os_str().as_encoded_bytes())
            .map_err(|_| format!("{} contains a NUL byte", directory.display()))?;
        let inotify = ququmatz::Inotify::new().map_err(|e| format!("creating inotify: {e}"))?;
        inotify
            .add_watch(
                &target,
                ququmatz::WatchMask::CREATE | ququmatz::WatchMask::MOVED_TO,
            )
            .map_err(|e| format!("watching {}: {e}", directory.display()))?;
        Ok(Self {
            inotify,
            socket_name,
        })
    }

    /// Forget events queued so far, so the next wait reports only a socket
    /// created after this call.
    pub fn discard_pending(&self) {
        InotifyEvents::poll_events(&self.inotify, |_| {});
    }

    /// Block until the socket file is created.
    pub fn wait_for_socket(&self) {
        loop {
            let _ = Poller::one(self.inotify.fd().as_i32()).poll(Self::BLOCK_FOREVER);
            if self.socket_created() {
                return;
            }
        }
    }

    /// Drain pending events; whether any of them created the socket file.
    /// A queue overflow counts, since events were lost.
    pub fn socket_created(&self) -> bool {
        let mut created = false;
        InotifyEvents::poll_events(&self.inotify, |touched| {
            created |= touched.mask & Self::Q_OVERFLOW != 0
                || touched.name == Some(self.socket_name.as_encoded_bytes());
        });
        created
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("bus_socket_watch_{}_{label}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create scratch directory");
            Self(path)
        }

        fn socket(&self) -> PathBuf {
            self.0.join("bus")
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn creating_the_socket_is_reported() {
        let dir = ScratchDir::new("created");
        let watch = BusSocketWatch::arm_on(&dir.socket()).expect("arm");
        std::fs::write(dir.socket(), b"").expect("create socket stand-in");
        assert!(watch.socket_created());
    }

    #[test]
    fn a_report_is_not_repeated() {
        let dir = ScratchDir::new("once");
        let watch = BusSocketWatch::arm_on(&dir.socket()).expect("arm");
        std::fs::write(dir.socket(), b"").expect("create socket stand-in");
        assert!(watch.socket_created());
        assert!(!watch.socket_created());
    }

    #[test]
    fn other_files_in_the_directory_are_ignored() {
        let dir = ScratchDir::new("other");
        let watch = BusSocketWatch::arm_on(&dir.socket()).expect("arm");
        std::fs::write(dir.0.join("not-the-bus"), b"").expect("create other file");
        assert!(!watch.socket_created());
    }

    #[test]
    fn renaming_the_socket_into_place_is_reported() {
        let dir = ScratchDir::new("renamed");
        let watch = BusSocketWatch::arm_on(&dir.socket()).expect("arm");
        let staging = dir.0.join("bus.tmp");
        std::fs::write(&staging, b"").expect("create staging file");
        std::fs::rename(&staging, dir.socket()).expect("rename into place");
        assert!(watch.socket_created());
    }

    #[test]
    fn discarded_events_are_not_reported() {
        let dir = ScratchDir::new("discard");
        let watch = BusSocketWatch::arm_on(&dir.socket()).expect("arm");
        std::fs::write(dir.socket(), b"").expect("create socket stand-in");
        watch.discard_pending();
        assert!(!watch.socket_created());
    }

    #[test]
    fn a_socket_created_while_blocked_ends_the_wait() {
        let dir = ScratchDir::new("blocked");
        let watch = BusSocketWatch::arm_on(&dir.socket()).expect("arm");
        let socket = dir.socket();
        let creator = std::thread::spawn(move || std::fs::write(socket, b"").expect("create"));
        watch.wait_for_socket();
        creator.join().expect("creator thread");
    }

    #[test]
    fn a_missing_directory_cannot_be_watched() {
        let missing = std::env::temp_dir()
            .join(format!("bus_socket_watch_{}_missing", std::process::id()))
            .join("bus");
        assert!(BusSocketWatch::arm_on(&missing).is_err());
    }
}
