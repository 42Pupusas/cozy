//! [`Sidebar`] -- the places pane down the left edge.
//!
//! Three sections: the XDG user directories ("Places"), the user's own
//! bookmarks ("Favorites"), and attached removable storage ("Devices"). This
//! module owns the *model* -- what rows exist, which is highlighted, what a
//! click resolves to -- and knows nothing about pixels. `window.rs` paints it.
//!
//! Devices come last deliberately: it is the only section that appears and
//! disappears on its own, and a section that can grow by two rows without
//! the user doing anything must not push the rows they were aiming at.
//!
//! The flattened-row representation is the point: a sidebar is visually two
//! lists with headings, but for hit-testing and keyboard navigation it is one
//! list of rows, some of which happen not to be selectable. Building that
//! flattening once, here, keeps the painter from re-deriving section offsets
//! and getting them subtly wrong.

use std::path::{Path, PathBuf};

use crate::favorites::Favorites;
use user_dirs::{UserDir, UserDirs};
use volumes::{DeviceScan, VolumeSet};

/// One row in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarRow {
    /// A section heading -- rendered in the small caps-ish label style and
    /// never selectable.
    Heading(&'static str),
    /// A navigable place.
    Place {
        label: String,
        path: PathBuf,
        /// Whether this row came from the favorites section, so the painter
        /// can mark it and the keymap can offer "remove bookmark".
        favorite: bool,
    },
    /// An attached removable volume.
    ///
    /// Distinct from [`Self::Place`] rather than reusing it with a flag: a
    /// device carries a detail line the painter shows, it can be flushed,
    /// and it can vanish while the pointer is over it. Those are different
    /// enough behaviours that folding them into `Place` would mean three
    /// booleans and a `match` at every use.
    Device {
        label: String,
        path: PathBuf,
        /// The filesystem and read-only state, e.g. `vfat, read-only`.
        detail: String,
        /// Whether the volume refuses writes, so the painter can mark it.
        read_only: bool,
    },
    /// A removable device that is plugged in but not mounted.
    ///
    /// A third variant rather than an `Option<PathBuf>` on [`Self::Device`],
    /// because the difference is not cosmetic: this row has **no path**, so
    /// it cannot navigate, cannot be flushed, and cannot be the highlighted
    /// current directory. Making the path optional would push a `None` check
    /// into every one of those call sites, and the one that forgot it would
    /// navigate the browser to a directory that does not exist.
    Unmounted {
        label: String,
        /// The `/dev` path, which names the device to mount.
        source: String,
        /// The filesystem and size, e.g. `vfat, 1.9g`.
        detail: String,
    },
}

impl SidebarRow {
    /// The path this row navigates to, if any.
    ///
    /// `None` for an unmounted device: it has no directory to open, and
    /// returning any path for it would send the browser somewhere that does
    /// not exist.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Place { path, .. } | Self::Device { path, .. } => Some(path.as_path()),
            Self::Heading(_) | Self::Unmounted { .. } => None,
        }
    }

    /// Whether the row can be clicked or moved onto.
    ///
    /// An unmounted device is selectable even though it navigates nowhere:
    /// clicking it is how the user asks to mount it, and a row that could
    /// not be clicked would offer no way to do that.
    #[must_use]
    pub const fn selectable(&self) -> bool {
        matches!(
            self,
            Self::Place { .. } | Self::Device { .. } | Self::Unmounted { .. }
        )
    }

    /// Whether this row is a mounted removable device.
    ///
    /// False for an unmounted one: the flush action reaches devices through
    /// this test, and flushing a device that is not attached would act on
    /// nothing -- or, with a path, on the wrong thing.
    #[must_use]
    pub const fn is_device(&self) -> bool {
        matches!(self, Self::Device { .. })
    }

    /// The `/dev` path of an unmounted device, which names what to mount.
    #[must_use]
    pub const fn mountable_source(&self) -> Option<&str> {
        match self {
            Self::Unmounted { source, .. } => Some(source.as_str()),
            _ => None,
        }
    }

    /// The device's detail line, if this row carries one.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Device { detail, .. } | Self::Unmounted { detail, .. } => {
                Some(detail.as_str()).filter(|detail| !detail.is_empty())
            }
            _ => None,
        }
    }
}

/// The places pane: sections, rows, and which row the current directory is.
pub struct Sidebar {
    rows: Vec<SidebarRow>,
    /// Whether the pane is shown at all.
    visible: bool,
}

impl Sidebar {
    /// Heading for the XDG places section.
    const PLACES: &'static str = "Places";
    /// Heading for the bookmarks section.
    const FAVORITES: &'static str = "Favorites";
    /// Heading for the removable storage section.
    const DEVICES: &'static str = "Devices";
    /// The directories this sidebar offers, in display order.
    ///
    /// A subset of the spec's eight, in the order people reach for them
    /// rather than the order the file lists them. `Templates` and
    /// `PublicShare` are omitted: both are session plumbing that a file
    /// browser's "places" list would only clutter.
    const PLACES_ORDER: [UserDir; 6] = [
        UserDir::Desktop,
        UserDir::Documents,
        UserDir::Download,
        UserDir::Music,
        UserDir::Pictures,
        UserDir::Videos,
    ];

    /// Build the sidebar with no devices attached.
    #[must_use]
    pub fn new(dirs: &UserDirs, favorites: &Favorites) -> Self {
        Self::with_volumes(dirs, favorites, &VolumeSet::default())
    }

    /// Build the sidebar with only mounted devices listed.
    #[must_use]
    pub fn with_volumes(dirs: &UserDirs, favorites: &Favorites, volumes: &VolumeSet) -> Self {
        Self::with_storage(dirs, favorites, volumes, &DeviceScan::default())
    }

    /// Build the sidebar from the user's directories, bookmarks and storage.
    ///
    /// `volumes` are the mounted filesystems and `devices` is every removable
    /// device attached. The two overlap, so the mounted rows are taken from
    /// `volumes` and only the *unmounted* devices are taken from `devices` --
    /// listing a mounted stick from both would show one device twice, once
    /// offering to mount what is already mounted.
    ///
    /// The mounted half comes from `volumes` rather than the scan because a
    /// mount entry carries what the scan cannot: the read-only state, and a
    /// label resolved through `/dev/disk/by-label`.
    #[must_use]
    pub fn with_storage(
        dirs: &UserDirs,
        favorites: &Favorites,
        volumes: &VolumeSet,
        devices: &DeviceScan,
    ) -> Self {
        let mut rows = vec![SidebarRow::Heading(Self::PLACES)];
        // Home leads the list: it is where the browser opens and where people
        // navigate back to most.
        rows.push(SidebarRow::Place {
            label: "Home".to_owned(),
            path: dirs.home().to_path_buf(),
            favorite: false,
        });
        let existing = dirs.existing();
        for dir in Self::PLACES_ORDER {
            let Some((_, path)) = existing.iter().find(|(d, _)| *d == dir) else {
                continue;
            };
            // `$HOME` itself is a legal value for a user directory (it means
            // "I have no separate Desktop"); listing it again under another
            // name would be a confusing duplicate of the Home row.
            if path == dirs.home() {
                continue;
            }
            rows.push(SidebarRow::Place {
                label: dir.label().to_owned(),
                path: path.clone(),
                favorite: false,
            });
        }

        if !favorites.is_empty() {
            rows.push(SidebarRow::Heading(Self::FAVORITES));
            for path in favorites.paths() {
                rows.push(SidebarRow::Place {
                    label: Favorites::label(path),
                    path: path.clone(),
                    favorite: true,
                });
            }
        }

        // A section with nothing in it is omitted rather than shown as an
        // empty heading.
        let unmounted = devices.mountable();
        if !volumes.is_empty() || !unmounted.is_empty() {
            rows.push(SidebarRow::Heading(Self::DEVICES));
            for volume in volumes.volumes() {
                rows.push(SidebarRow::Device {
                    label: volume.label().to_owned(),
                    path: volume.mount_point().to_path_buf(),
                    detail: volume.detail(),
                    read_only: volume.read_only(),
                });
            }
            // Unmounted devices sit below the mounted ones, for the same
            // reason the whole section sits last: this is the group that
            // appears and disappears on its own, so it must not push the
            // rows the user was aiming at.
            for device in unmounted {
                rows.push(SidebarRow::Unmounted {
                    label: device.display_name().to_owned(),
                    source: device.source().to_owned(),
                    detail: String::new(),
                });
            }
        }

        Self {
            rows,
            visible: true,
        }
    }

    /// The device row mounted at `path`, if any.
    ///
    /// The flush action reaches devices by row index, since that is what a
    /// click resolves to; this is the by-path lookup the tests use to assert
    /// that a plain place is never mistaken for a device.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn device_at(&self, path: &Path) -> Option<&SidebarRow> {
        self.rows
            .iter()
            .find(|row| row.is_device() && row.path() == Some(path))
    }

    /// Every row, headings included, top to bottom.
    #[must_use]
    pub fn rows(&self) -> &[SidebarRow] {
        &self.rows
    }

    /// Whether the pane is shown.
    #[must_use]
    pub const fn visible(&self) -> bool {
        self.visible
    }

    /// Show or hide the pane. Returns the new visibility.
    pub const fn toggle(&mut self) -> bool {
        self.visible = !self.visible;
        self.visible
    }

    /// The row index whose path is `path`, for highlighting where we are.
    ///
    /// Prefers the favorites section on a tie: if a directory is both an XDG
    /// place and bookmarked, the bookmark is the more specific statement of
    /// intent.
    #[must_use]
    pub fn index_of(&self, path: &Path) -> Option<usize> {
        let mut found = None;
        for (i, row) in self.rows.iter().enumerate() {
            if row.path() == Some(path) {
                let favorite = matches!(row, SidebarRow::Place { favorite: true, .. });
                if favorite {
                    return Some(i);
                }
                found = found.or(Some(i));
            }
        }
        found
    }

    /// The index of the row at a vertical offset, given a uniform row height.
    ///
    /// Returns `None` above the pane, past the last row, or on a heading --
    /// clicking a section title must do nothing rather than jump somewhere
    /// arbitrary.
    ///
    /// Yields the *index* rather than the row because the painter walks rows
    /// by position and asks "is this the hovered one", so it compares indices;
    /// a caller wanting the row itself indexes [`Self::rows`]. Both the click
    /// and the hover path go through this one function deliberately -- two
    /// copies of the arithmetic would be free to drift by a row, lighting one
    /// place while clicking another.
    #[must_use]
    pub fn row_index_at(&self, y: i32, row_height: i32) -> Option<usize> {
        if y < 0 || row_height <= 0 {
            return None;
        }
        let index = usize::try_from(y / row_height).ok()?;
        self.rows.get(index).filter(|r| r.selectable())?;
        Some(index)
    }
}

#[cfg(test)]
mod tests {
    use super::{Sidebar, SidebarRow};
    use crate::favorites::Favorites;
    use crate::test_support::TempDir;
    use std::path::Path;
    use user_dirs::UserDirs;
    use volumes::{AttachedDevice, DeviceClass, DeviceScan, Volume, VolumeSet};

    /// A stick that is plugged in and not mounted.
    fn unmounted(label: &str) -> AttachedDevice {
        AttachedDevice::new("/dev/sdb1", Some(label), "vfat", 1_000_000_000, 8, 17)
    }

    /// A USB stick attached at `/run/media/user/<label>`.
    fn stick(label: &str, read_only: bool) -> Volume {
        Volume::new(
            label,
            format!("/run/media/user/{label}"),
            "/dev/sdb1",
            "vfat",
            DeviceClass::Removable,
            read_only,
        )
    }

    /// Build a sidebar over a real temp directory with a couple of XDG dirs.
    fn fixture(tmp: &TempDir) -> (UserDirs, Favorites) {
        tmp.create_dir("Documents");
        tmp.create_dir("Downloads");
        let doc = format!(
            "XDG_DOCUMENTS_DIR=\"{home}/Documents\"\nXDG_DOWNLOAD_DIR=\"{home}/Downloads\"\n",
            home = tmp.path().display()
        );
        (UserDirs::parse(&doc, tmp.path()), Favorites::new())
    }

    /// Every heading in the bar, in order.
    fn headings(bar: &Sidebar) -> Vec<&'static str> {
        bar.rows()
            .iter()
            .filter_map(|r| match r {
                SidebarRow::Heading(h) => Some(*h),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn both_sections_are_present() {
        let tmp = TempDir::new("sidebar-sections");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        assert_eq!(headings(&bar), ["Places"]);
    }

    #[test]
    fn the_favorites_section_appears_once_something_is_bookmarked() {
        let tmp = TempDir::new("sidebar-sections-fav");
        let (dirs, mut fav) = fixture(&tmp);
        fav.add(Path::new("/mnt/work"));
        let bar = Sidebar::new(&dirs, &fav);
        assert_eq!(headings(&bar), ["Places", "Favorites"]);
    }

    /// With nothing plugged in there is no section at all -- not an empty
    /// heading, which would be chrome describing nothing.
    #[test]
    fn the_devices_section_is_absent_when_nothing_is_attached() {
        let tmp = TempDir::new("sidebar-nodev");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::with_volumes(&dirs, &fav, &VolumeSet::default());
        assert!(!headings(&bar).contains(&"Devices"));
        assert!(!bar.rows().iter().any(SidebarRow::is_device));
    }

    #[test]
    fn an_attached_device_appears_under_a_devices_heading() {
        let tmp = TempDir::new("sidebar-dev");
        let (dirs, fav) = fixture(&tmp);
        let volumes = VolumeSet::of(vec![stick("KINGSTON", false)]);
        let bar = Sidebar::with_volumes(&dirs, &fav, &volumes);
        assert_eq!(headings(&bar), ["Places", "Devices"]);
        let rows = bar.rows();
        let heading = rows
            .iter()
            .position(|r| matches!(r, SidebarRow::Heading("Devices")))
            .expect("the devices heading");
        let device = rows
            .iter()
            .position(SidebarRow::is_device)
            .expect("the stick");
        assert!(device > heading, "a device must sit under its heading");
    }

    /// Devices come last so that a stick appearing on its own cannot push
    /// the row the user was reaching for.
    #[test]
    fn attaching_a_device_does_not_move_the_rows_above_it() {
        let tmp = TempDir::new("sidebar-stable");
        let (dirs, fav) = fixture(&tmp);
        let before = Sidebar::new(&dirs, &fav);
        let paths: Vec<_> = before.rows().iter().map(SidebarRow::path).collect();
        let volumes = VolumeSet::of(vec![stick("KINGSTON", false)]);
        let after = Sidebar::with_volumes(&dirs, &fav, &volumes);
        let grown: Vec<_> = after
            .rows()
            .iter()
            .take(paths.len())
            .map(SidebarRow::path)
            .collect();
        assert_eq!(paths, grown, "existing rows must keep their positions");
    }

    /// A device row navigates like any other place.
    #[test]
    fn a_device_row_is_selectable_and_carries_its_mount_point() {
        let tmp = TempDir::new("sidebar-devnav");
        let (dirs, fav) = fixture(&tmp);
        let volumes = VolumeSet::of(vec![stick("KINGSTON", false)]);
        let bar = Sidebar::with_volumes(&dirs, &fav, &volumes);
        let row = bar
            .rows()
            .iter()
            .find(|r| r.is_device())
            .expect("the stick");
        assert!(row.selectable());
        assert_eq!(row.path(), Some(Path::new("/run/media/user/KINGSTON")));
    }

    #[test]
    fn a_device_carries_a_detail_line_and_a_place_does_not() {
        let tmp = TempDir::new("sidebar-detail");
        let (dirs, fav) = fixture(&tmp);
        let volumes = VolumeSet::of(vec![stick("DISC", true)]);
        let bar = Sidebar::with_volumes(&dirs, &fav, &volumes);
        let device = bar
            .device_at(Path::new("/run/media/user/DISC"))
            .expect("found");
        assert_eq!(device.detail(), Some("vfat, read-only"));
        let home = bar
            .rows()
            .iter()
            .find(|r| r.path() == Some(tmp.path()))
            .expect("home");
        assert_eq!(home.detail(), None);
    }

    /// The browsing directory must never be mistaken for a device, or the
    /// flush action would offer to act on the root filesystem.
    #[test]
    fn a_plain_place_is_not_reported_as_a_device() {
        let tmp = TempDir::new("sidebar-notdev");
        let (dirs, fav) = fixture(&tmp);
        let volumes = VolumeSet::of(vec![stick("KINGSTON", false)]);
        let bar = Sidebar::with_volumes(&dirs, &fav, &volumes);
        assert!(bar.device_at(tmp.path()).is_none());
        assert!(bar.device_at(Path::new("/")).is_none());
    }

    /// The open directory highlights whether it is a place or a device.
    #[test]
    fn an_open_device_is_locatable_for_highlighting() {
        let tmp = TempDir::new("sidebar-devhl");
        let (dirs, fav) = fixture(&tmp);
        let volumes = VolumeSet::of(vec![stick("KINGSTON", false)]);
        let bar = Sidebar::with_volumes(&dirs, &fav, &volumes);
        let index = bar
            .index_of(Path::new("/run/media/user/KINGSTON"))
            .expect("the open device highlights");
        assert!(bar.rows()[index].is_device());
    }

    /// Two sticks must both appear; the count is what tells a user whether
    /// the machine saw the one they just plugged in.
    #[test]
    fn every_attached_device_is_listed() {
        let tmp = TempDir::new("sidebar-many");
        let (dirs, fav) = fixture(&tmp);
        let volumes = VolumeSet::of(vec![stick("ALPHA", false), stick("BETA", true)]);
        let bar = Sidebar::with_volumes(&dirs, &fav, &volumes);
        assert_eq!(bar.rows().iter().filter(|r| r.is_device()).count(), 2);
    }

    /// The row this whole feature exists for: a stick that is plugged in but
    /// not mounted must be listed, so there is something to click.
    #[test]
    fn an_unmounted_device_is_listed_under_devices() {
        let tmp = TempDir::new("sidebar-unmounted");
        let (dirs, fav) = fixture(&tmp);
        let devices = DeviceScan::of(vec![unmounted("KINGSTON")]);
        let bar = Sidebar::with_storage(&dirs, &fav, &VolumeSet::default(), &devices);
        assert_eq!(headings(&bar), ["Places", "Devices"]);
        let row = bar
            .rows()
            .iter()
            .find(|r| r.mountable_source().is_some())
            .expect("the unmounted stick");
        assert_eq!(row.mountable_source(), Some("/dev/sdb1"));
        assert!(row.selectable(), "clicking it is how a mount is asked for");
    }

    /// The dangerous case: an unmounted device has nowhere to go, and a path
    /// on this row would navigate the browser into a directory that does not
    /// exist.
    #[test]
    fn an_unmounted_device_navigates_nowhere_and_cannot_be_flushed() {
        let tmp = TempDir::new("sidebar-unmounted-nopath");
        let (dirs, fav) = fixture(&tmp);
        let devices = DeviceScan::of(vec![unmounted("KINGSTON")]);
        let bar = Sidebar::with_storage(&dirs, &fav, &VolumeSet::default(), &devices);
        let row = bar
            .rows()
            .iter()
            .find(|r| r.mountable_source().is_some())
            .expect("the stick");
        assert_eq!(row.path(), None, "there is no directory to open");
        assert!(
            !row.is_device(),
            "flushing an unmounted device acts on nothing"
        );
    }

    /// A mounted stick appears once, as a mounted row. Listing it from both
    /// sources would show one device twice, one of which offers to mount
    /// what is already mounted.
    #[test]
    fn a_mounted_device_is_not_also_listed_as_mountable() {
        let tmp = TempDir::new("sidebar-nodupe");
        let (dirs, fav) = fixture(&tmp);
        let volumes = VolumeSet::of(vec![stick("KINGSTON", false)]);
        let devices = DeviceScan::of(vec![
            unmounted("KINGSTON").mounted_at("/run/media/user/KINGSTON"),
        ]);
        let bar = Sidebar::with_storage(&dirs, &fav, &volumes, &devices);
        assert_eq!(
            bar.rows()
                .iter()
                .filter(|r| r.mountable_source().is_some())
                .count(),
            0,
            "a mounted device offers no mount action"
        );
        assert_eq!(bar.rows().iter().filter(|r| r.is_device()).count(), 1);
    }

    /// The section must appear for an unmounted device alone -- otherwise a
    /// stick plugged into a machine with nothing else attached is invisible.
    #[test]
    fn the_devices_section_appears_for_an_unmounted_device_alone() {
        let tmp = TempDir::new("sidebar-onlyunmounted");
        let (dirs, fav) = fixture(&tmp);
        let devices = DeviceScan::of(vec![unmounted("KINGSTON")]);
        let bar = Sidebar::with_storage(&dirs, &fav, &VolumeSet::default(), &devices);
        assert!(headings(&bar).contains(&"Devices"));
    }

    #[test]
    fn an_unmounted_device_carries_no_instruction_caption() {
        let tmp = TempDir::new("sidebar-nocaption");
        let (dirs, fav) = fixture(&tmp);
        let devices = DeviceScan::of(vec![unmounted("KINGSTON")]);
        let bar = Sidebar::with_storage(&dirs, &fav, &VolumeSet::default(), &devices);
        let row = bar
            .rows()
            .iter()
            .find(|r| r.mountable_source().is_some())
            .expect("the stick");
        assert_eq!(row.detail(), None);
    }

    /// Mounted devices keep their positions when an unmounted one appears.
    #[test]
    fn an_unmounted_device_sits_below_the_mounted_ones() {
        let tmp = TempDir::new("sidebar-order");
        let (dirs, fav) = fixture(&tmp);
        let volumes = VolumeSet::of(vec![stick("MOUNTED", false)]);
        let devices = DeviceScan::of(vec![unmounted("LOOSE")]);
        let bar = Sidebar::with_storage(&dirs, &fav, &volumes, &devices);
        let mounted = bar
            .rows()
            .iter()
            .position(SidebarRow::is_device)
            .expect("mounted");
        let loose = bar
            .rows()
            .iter()
            .position(|r| r.mountable_source().is_some())
            .expect("unmounted");
        assert!(loose > mounted);
    }

    /// A plain place must never be mistaken for something to mount.
    #[test]
    fn a_plain_place_offers_no_mount_source() {
        let tmp = TempDir::new("sidebar-placenomount");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        assert!(bar.rows().iter().all(|r| r.mountable_source().is_none()));
    }

    #[test]
    fn home_leads_the_places_section() {
        let tmp = TempDir::new("sidebar-home");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        let first = bar.rows().iter().find(|r| r.selectable()).expect("a place");
        assert_eq!(first.path(), Some(tmp.path()));
    }

    /// Only directories that exist may appear.
    #[test]
    fn a_missing_user_directory_is_not_listed() {
        let tmp = TempDir::new("sidebar-missing");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        assert!(
            !bar.rows()
                .iter()
                .any(|r| r.path() == Some(&tmp.path().join("Music"))),
            "Music was never created, so it must not be offered"
        );
    }

    #[test]
    fn an_empty_favorites_section_is_omitted_with_no_instruction_in_its_place() {
        let tmp = TempDir::new("sidebar-no-favorites");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        assert!(
            !bar.rows()
                .iter()
                .any(|r| matches!(r, SidebarRow::Heading("Favorites")))
        );
    }

    #[test]
    fn bookmarks_appear_under_the_favorites_heading() {
        let tmp = TempDir::new("sidebar-favs");
        let (dirs, mut fav) = fixture(&tmp);
        fav.add(Path::new("/mnt/work"));
        let bar = Sidebar::new(&dirs, &fav);
        let rows = bar.rows();
        let heading = rows
            .iter()
            .position(|r| matches!(r, SidebarRow::Heading("Favorites")))
            .expect("favorites heading");
        let entry = rows
            .iter()
            .position(|r| r.path() == Some(Path::new("/mnt/work")))
            .expect("the bookmark");
        assert!(entry > heading, "a bookmark must sit under its heading");
        assert!(matches!(
            rows[entry],
            SidebarRow::Place { favorite: true, .. }
        ));
    }

    /// A heading is not a destination; clicking one must do nothing.
    #[test]
    fn a_heading_is_never_selectable() {
        let tmp = TempDir::new("sidebar-heading");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        assert!(!bar.rows()[0].selectable());
        assert_eq!(
            bar.row_index_at(0, 20),
            None,
            "clicking the heading resolves to nothing"
        );
    }

    #[test]
    fn a_click_resolves_to_the_row_under_it() {
        let tmp = TempDir::new("sidebar-click");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        // Row 1 is Home (row 0 being the "Places" heading).
        let index = bar.row_index_at(25, 20).expect("a place at row 1");
        assert_eq!(bar.rows()[index].path(), Some(tmp.path()));
    }

    #[test]
    fn a_click_past_the_last_row_resolves_to_nothing() {
        let tmp = TempDir::new("sidebar-past");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        assert_eq!(bar.row_index_at(10_000, 20), None);
    }

    /// Degenerate geometry must not panic or divide by zero.
    #[test]
    fn a_zero_row_height_is_refused_rather_than_dividing_by_zero() {
        let tmp = TempDir::new("sidebar-zero");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        assert_eq!(bar.row_index_at(5, 0), None);
        assert_eq!(bar.row_index_at(-5, 20), None);
    }

    #[test]
    fn the_current_directory_is_locatable_for_highlighting() {
        let tmp = TempDir::new("sidebar-current");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        assert!(bar.index_of(tmp.path()).is_some());
        assert_eq!(bar.index_of(Path::new("/nowhere")), None);
    }

    /// A directory that is both an XDG place and a bookmark highlights as the
    /// bookmark -- the more specific statement of the user's intent.
    #[test]
    fn a_bookmarked_place_highlights_in_the_favorites_section() {
        let tmp = TempDir::new("sidebar-tie");
        let (dirs, mut fav) = fixture(&tmp);
        let docs = tmp.path().join("Documents");
        fav.add(&docs);
        let bar = Sidebar::new(&dirs, &fav);
        let index = bar.index_of(&docs).expect("found");
        assert!(matches!(
            bar.rows()[index],
            SidebarRow::Place { favorite: true, .. }
        ));
    }

    /// `XDG_DESKTOP_DIR="$HOME"` is the documented "no desktop dir" value; it
    /// must not produce a second row duplicating Home.
    #[test]
    fn a_user_directory_equal_to_home_does_not_duplicate_the_home_row() {
        let tmp = TempDir::new("sidebar-dup");
        let doc = format!("XDG_DESKTOP_DIR=\"{}\"\n", tmp.path().display());
        let dirs = UserDirs::parse(&doc, tmp.path());
        let bar = Sidebar::new(&dirs, &Favorites::new());
        let count = bar
            .rows()
            .iter()
            .filter(|r| r.path() == Some(tmp.path()))
            .count();
        assert_eq!(count, 1, "Home must appear exactly once");
    }

    #[test]
    fn toggling_hides_and_shows_the_pane() {
        let tmp = TempDir::new("sidebar-toggle");
        let (dirs, fav) = fixture(&tmp);
        let mut bar = Sidebar::new(&dirs, &fav);
        assert!(bar.visible());
        assert!(!bar.toggle());
        assert!(!bar.visible());
        assert!(bar.toggle());
    }
}
