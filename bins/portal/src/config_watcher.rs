//! Watches `config::Config::xdg_config_path()` for saves and reports when
//! it's time to reload.
//!
//! A near-copy of `bins/terminal`'s own `ConfigWatcher` (same directory
//! watch + filename filter, same inotify ABI parsing), *not* shared code:
//! `docs/portal-scoping.md`'s own delivery-sequence note says to refactor
//! the statusbar watcher into `dbus-server` "only if it reduces
//! duplication without mixing service policy into generic code" -- the
//! terminal's copy applies a reload straight to a live `TermWindow`
//! (rendering/cursor-animation policy), where this one only needs to
//! report *that* a reload should happen and hand back the freshly parsed
//! `config::Config` for [`crate::portal_loop::PortalLoop`] to apply via
//! [`crate::settings_state::SettingsState::reload`]. Two small, distinct
//! consumers of the same kernel mechanism don't yet justify a shared
//! abstraction over "what changed" -- that's a judgment call to revisit
//! if a third caller appears.

/// One directory-wide inotify watch, filtered down to saves of a single
/// file.
pub struct ConfigWatcher {
    /// `None` if the config directory doesn't exist yet or inotify setup
    /// failed -- live-reload is then simply unavailable, not an error
    /// (see [`Self::new`]).
    inotify: Option<ququmatz::Inotify>,
    /// The config file's bare name (no directory), to filter the
    /// directory-wide watch down to saves of *this* file specifically.
    file_name: Option<String>,
    path: std::path::PathBuf,
}

impl ConfigWatcher {
    /// Set up the watch, best-effort: if the config dir doesn't exist yet
    /// (nothing has ever been saved there) or inotify setup fails,
    /// live-reload is simply unavailable and the rest of the daemon works
    /// exactly as if this watcher were never consulted.
    ///
    /// Watches the config file for writes (most editors save via
    /// rename-into-place; watching the *directory* for
    /// `CLOSE_WRITE|MOVED_TO` on any entry and filtering by filename
    /// catches every common save strategy, where watching the file
    /// itself would miss an atomic rename replacing the inode out from
    /// under the watch).
    #[must_use]
    pub fn new() -> Self {
        Self::for_path(config::Config::xdg_config_path())
    }

    /// Same as [`Self::new`], but watching an arbitrary `path` instead of
    /// the real XDG config location -- what lets
    /// `tests/real_bus_probe.rs` prove the whole reload pipeline (file
    /// save -> reparse -> `SettingChanged` over a real bus) against a
    /// throwaway temp directory instead of racing every other test in
    /// the process over a shared, global `$XDG_CONFIG_HOME`.
    #[must_use]
    pub fn for_path(path: std::path::PathBuf) -> Self {
        let inotify = path.parent().and_then(|dir| {
            let ino = ququmatz::Inotify::new().ok()?;
            let dir_c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).ok()?;
            ino.add_watch(
                &dir_c,
                ququmatz::WatchMask::CLOSE_WRITE | ququmatz::WatchMask::MOVED_TO,
            )
            .ok()?;
            Some(ino)
        });
        let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        Self {
            inotify,
            file_name,
            path,
        }
    }

    /// The raw inotify fd, for polling alongside the D-Bus socket's own
    /// fd -- `None` if the watch never got set up ([`Self::new`]'s
    /// best-effort fallback), in which case the caller simply never has
    /// anything to poll for on this source.
    #[must_use]
    pub fn raw_fd(&self) -> Option<i32> {
        use ququmatz::types::RawFd;
        self.inotify.as_ref().map(|ino| RawFd::as_i32(ino.fd()))
    }

    /// If our config file was touched since the last call, re-parse and
    /// return the fresh config. `Ok(None)` means nothing changed (the
    /// caller has nothing to do); `Err` means a save was detected but the
    /// file failed to parse, in which case the caller should keep
    /// whatever settings it already has rather than adopt a broken
    /// config.
    pub fn poll_reload(&self) -> Result<Option<config::Config>, config::ConfigError> {
        let (Some(ino), Some(fname)) = (&self.inotify, &self.file_name) else {
            return Ok(None);
        };
        if !Self::touched(ino, fname) {
            return Ok(None);
        }
        config::Config::load_from(&self.path).map(Some)
    }

    /// Drain every pending inotify event on the config directory's watch
    /// and report whether any of them named `fname` (our config file
    /// specifically, not some unrelated file saved alongside it in the
    /// same directory). Non-blocking -- the fd was opened with
    /// `IN_NONBLOCK`, so an empty read just means "nothing changed", not
    /// a stall.
    fn touched(ino: &ququmatz::Inotify, fname: &str) -> bool {
        // Sized for a handful of coalesced events; `inotify_event` is 16
        // bytes plus each event's (padded) trailing filename, so this
        // comfortably covers a burst of saves without needing to loop
        // reads.
        let mut buf = [0u8; 4096];
        let Ok(n) = ququmatz::syscall::read(ino.fd(), buf.as_mut_ptr(), buf.len()) else {
            return false;
        };
        let header_size = std::mem::size_of::<ququmatz::InotifyEvent>();
        let mut touched = false;
        let mut off = 0usize;
        while off + header_size <= n {
            // SAFETY: `buf[off..]` holds at least `header_size` bytes of a
            // kernel-written `inotify_event`, which is a plain-old-data
            // repr(C) struct with no padding-sensitive invariants -- an
            // unaligned read is exactly what the kernel's own byte-packed
            // event stream requires.
            let ev: ququmatz::InotifyEvent =
                unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off).cast()) };
            let name_len = ev.len as usize;
            let name_start = off + header_size;
            let name_end = name_start + name_len;
            if name_end > n {
                break; // Truncated trailing event -- stop rather than read garbage.
            }
            if name_len > 0 {
                // The kernel pads the name with trailing NULs to the next
                // 4-byte boundary; trim them before comparing.
                let raw = &buf[name_start..name_end];
                let trimmed = &raw[..raw.iter().position(|&b| b == 0).unwrap_or(raw.len())];
                if trimmed == fname.as_bytes() {
                    touched = true;
                }
            }
            off = name_end;
        }
        touched
    }
}

impl Default for ConfigWatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::ConfigWatcher;

    /// Write to a file inside a fresh temp dir, watched the same way
    /// `ConfigWatcher::new` watches the real config directory
    /// (`CLOSE_WRITE | MOVED_TO` on the directory, filtered by filename),
    /// and confirm `ConfigWatcher::touched` reports the save. Exercises
    /// the real kernel ABI -- event header size/layout, name
    /// padding/NUL-trimming -- not a mock. Mirrors `bins/terminal`'s own
    /// identical test.
    #[test]
    fn detects_a_save_to_the_watched_file() {
        let dir =
            std::env::temp_dir().join(format!("portal-live-reload-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("config.toml");
        std::fs::write(&file, "initial").unwrap();

        let ino = ququmatz::Inotify::new().expect("inotify_init1");
        let dir_c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).unwrap();
        ino.add_watch(
            &dir_c,
            ququmatz::WatchMask::CLOSE_WRITE | ququmatz::WatchMask::MOVED_TO,
        )
        .expect("add_watch");

        assert!(!ConfigWatcher::touched(&ino, "config.toml"));

        std::fs::write(&file, "updated").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(ConfigWatcher::touched(&ino, "config.toml"));

        assert!(!ConfigWatcher::touched(&ino, "config.toml"));

        std::fs::write(dir.join("unrelated.txt"), "noise").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!ConfigWatcher::touched(&ino, "config.toml"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
