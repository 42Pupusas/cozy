# Scoping: a native file browser (`bins/files`)

Status: **scoping only**, no code written.
Sibling doc: `docs/portal-scoping.md` (deferred — this work is a prerequisite for
most of it anyway).

---

## 0. Why this is a much better target than the portal

The portal was blocked on four *infrastructure* gaps in `crates/dbus` (generic
marshaller, fd passing, object router, async multiplexing) before a single pixel
could be drawn. A file browser is blocked on **none** of them. It is a Wayland
client that reads directories and draws rows — every hard dependency already
exists in this workspace and is proven in a shipping bin.

Better still: `impl.portal.FileChooser` is the single most-used portal
interface, and it is *this widget in a dialog frame*. Building the file browser
first means the portal's hardest UI problem is already solved and battle-tested
when we come back to it.

---

## 1. What we already have (the reuse inventory)

I went through the workspace crate by crate. This is genuinely most of a file
manager already:

| Need | Existing asset | Confidence |
|---|---|---|
| Wayland connection bootstrap | `platform::surface::SurfaceBootstrap` | **Proven** — used by 4 bins |
| Double-buffered shm swap chain | `platform::present::PresentState` | **Proven** |
| Toplevel window (title, resize, close) | `platform::window::TermWindow` | Proven, but **terminal-shaped** — see §3.1 |
| Drawing primitives | `platform::draw::Canvas` — `fill_rect`, `draw_rect`, `hline`/`vline`, `draw_text`, `blit_rgba_scaled` | **Proven** |
| Font rasterisation + fallback | `crates/font`, `platform::text` | **Proven** |
| Text shaping / BiDi / line breaking | `crates/text_layout` | **Proven** |
| Keyboard input + repeat | `crates/xkb`, `platform::key_repeat` | **Proven** |
| Fuzzy filter ("type to find") | `launcher::fuzzy::FuzzyMatch` | **Proven** — lift as-is |
| Icon theme lookup | `launcher::icon_lookup::IconLookup` | Works, but **PNG-only / `hicolor`-only** — see §3.3 |
| PNG decode (icons, thumbnails) | `ixiptla-core` | **Proven** |
| "Open with" — `.desktop` catalog | `launcher::app_index`, `desktop_entry` | **Proven** |
| Launching a chosen app | `launcher::launch::ExecCommand` | Proven — but needs `%f`/`%U` *filled in*, see §3.4 |
| Live directory watching | `ququmatz::Inotify` | **Proven** — `bins/terminal` watches its config with it |
| Copy paths to clipboard | `crates/clipboard`, `clipboard-wayland` | **Proven** |
| Config + theming | `crates/config` (shelby/TOML) | **Proven** |
| Single-instance guard | `launcher::singleton` | Proven — reusable if we want one window |

The closest structural template is `bins/launcher`: a list of entries, a text
query, fuzzy ranking, icons blitted per row, keyboard navigation. A file browser
is that same loop pointed at `readdir` instead of `.desktop` files.

---

## 2. Scope: what "Nautilus feature set" actually means

Nautilus is ~200k lines. We should be explicit about the tiers rather than
pretend parity. My proposed cut:

### Tier 1 — the thing is useless without these
- Directory listing: name, size, mtime, type; `.` -hidden toggle
- Navigation: enter/up, back/forward history, path breadcrumb, `~`/`/` jumps
- Sorting: name / size / mtime / type, asc+desc
- Selection: single, ctrl-click, shift-range, select-all
- Open: run default app for a file, descend into a dir
- Type-to-filter using `launcher::fuzzy`
- Keyboard-first driving throughout (this workspace's house style)

### Tier 2 — what makes it a *file manager* and not a viewer
- Copy / cut / paste / move / rename / delete
- **Trash** (freedesktop Trash spec — `~/.local/share/Trash/{files,info}`, and
  the `.Trash-$uid` dance for other filesystems)
- New folder / new empty file
- Properties panel: permissions, owner, size-on-disk, mime type
- Recursive directory size ("calculate folder size")
- Progress + cancel for long operations
- Undo for destructive ops
- Icon grid view alongside list view

### Tier 3 — polish, genuinely nice, not launch-blocking
- Thumbnails for images (we have a PNG decoder; JPEG we do not — see §3.3)
- Bookmarks / sidebar places (`~/.config/gtk-3.0/bookmarks` is the de-facto file)
- Dual pane
- Tabs
- Search (recursive, with a live-updating result list)
- ~~Drag and drop~~ — **done** (`bins/files/src/drag_badge.rs`, in-window drops)
- ~~Mounted volume list~~ — **done** (`crates/volumes`, sidebar "Devices" section)

### Explicitly **out** of scope (delegate or skip)
- **Network/virtual filesystems** (GVfs — smb://, sftp://, mtp://, trash://).
  This is a whole daemon and IPC protocol. Local POSIX paths only.
- **Removable devices are now fully in scope and implemented.** This entry is
  kept as a record of two blockers that were claimed here and turned out not
  to hold.

  *Detection* (`crates/volumes`) reads the kernel's own tables —
  `/proc/self/mountinfo`, `/sys/class/block`, udev's blkid cache under
  `/run/udev/data`, `/dev/disk/by-label` — and needs no daemon at all, so it
  works on a machine with no UDisks2 installed, as this one is. It offers
  `syncfs(2)` ("Flush writes") rather than an eject, because that is the part
  that protects data and needs no privilege.

  *Mounting* (`crates/mounter`, `crates/polkit-agent`) starts a transient
  systemd `.mount` unit over the system bus and answers the polkit password
  prompt in-process.

  The first stale claim was that `crates/dbus` cannot marshal `a{sv}`. It
  marshals `a{sv}` today, and `bluez.rs` already walks `a{oa{sa{sv}}}` for
  `GetManagedObjects` — exactly UDisks2's shape.

  The second was that privilege is an absolute blocker. `mount(2)` does
  require `CAP_SYS_ADMIN`, and we still hold none — but we do not need it.
  systemd holds it, and polkit decides whether to lend it. What was actually
  needed was an *authentication agent*, which turned out not to require a
  setuid helper either: modern polkit accepts the PAM conversation over
  `/run/polkit/agent-helper.socket`.

  What remains genuinely out of reach is mounting **without a password**. That
  needs a scoped polkit rule, which is a system administration decision rather
  than a code change. `volumes::MountBackend` is the seam a different backend
  would fill.
- **File indexing / content search** (Tracker). Recursive walk is enough.
- **Archive integration**, batch rename, extensions/scripting.

---

## 3. The actual gaps — where the real work is

This is the honest part. Five things are genuinely missing, and two of them are
bigger than they look.

### 3.1 There is no reusable toplevel window — `TermWindow` is a terminal
`platform::window::TermWindow` does everything we want (xdg_toplevel, configure,
resize, title, decoration, clipboard) **but it is fused to the terminal**: it
carries grid/cell-geometry assumptions, and its configure path converts pixel
sizes into rows/columns. `bins/launcher` sidestepped this by writing its *own*
window (`LauncherWindow`, 675 lines) on layer-shell instead of xdg-shell.

A file browser needs a **normal, resizable, decorated toplevel** — layer-shell
is wrong (it's an overlay shell for bars/launchers). So we need a third window
implementation, and that's the moment to stop copy-pasting: extract a generic
`platform::toplevel::Toplevel` that `TermWindow` can eventually be rebuilt on.

> This is the single highest-value piece of the project. It's also the one
> most likely to balloon, because refactoring `TermWindow` onto it risks the
> terminal. **Recommendation: build `Toplevel` fresh, use it only for the file
> browser, and leave `TermWindow` untouched for now.** Converging them is a
> separate, later, well-tested task.

### 3.2 There is no widget layer at all
`Canvas` gives us `fill_rect` and `draw_text`. Everything above that — a
**scrollable virtualised list**, a text input with a cursor and selection, a
button, a context menu, a modal dialog, a resizable column header, a scrollbar —
does not exist. `bins/launcher` hand-rolls a fixed 10-row list with no
scrolling; `bins/statusbar` hand-rolls its own hit-testing.

A file manager needs real scrolling over 100k-entry directories, hover states,
and menus. This is a meaningful amount of work and it is the *second* long pole.
Scope it deliberately as a small retained-mode-free widget set — not a general
GUI toolkit, just the six widgets above.

### 3.3 Icon coverage will be the visible-quality bottleneck
`IconLookup` is **PNG-only and `hicolor`-only**, by explicit design (its own doc
comment says so). That was fine for a launcher: app icons are installed as PNG
into `hicolor` by spec.

It will **not** be fine here, for two reasons:
1. **MIME-type icons** (`text-x-generic`, `folder`, `video-x-generic`, …) are
   what a file browser draws hundreds of, and modern themes (Adwaita, Papirus,
   Breeze) ship those almost exclusively as **SVG**. PNG-only means most files
   render with no icon.
2. We don't parse `index.theme`, so we can't follow **theme inheritance** — the
   user's chosen theme won't resolve, only the `hicolor` fallback.

Three options, in ascending cost:
- **(a)** Ship a small built-in vector/bitmap icon set of our own, styled to
  match this workspace. Zero new parsing. Loses system-theme integration but
  gives a *consistent, deliberate* look — arguably on-brand for these bins.
- **(b)** Parse `index.theme` + add an SVG rasteriser. An SVG subset (paths,
  fills, transforms) is a real project; a full one is enormous.
- **(c)** Hybrid: (a) as the default, with `index.theme`+PNG lookup for themes
  that do ship PNG.

**Recommendation: (a) for v1.** It's the only option that doesn't bolt an SVG
renderer onto the critical path, and "styled to this workspace's other bins" —
which is what was actually asked for — argues *against* pulling in system icon
themes anyway.

Thumbnails: `ixiptla-core` is PNG-only, so image thumbnails work for PNG and
nothing else. JPEG decode is a separate (well-understood, but non-trivial)
project. Suggest: PNG thumbnails in Tier 3, JPEG deferred.

### 3.4 MIME detection and "open with" don't exist
We can list files but not say *what they are*. Needed:
- **Extension → MIME type** via `/usr/share/mime/` (the shared-mime-info
  `globs2` file is a simple text format — cheap and covers ~95% of real cases).
- **Content sniffing** via magic bytes for extensionless files (a modest
  built-in table covers the common ones; the full `magic` XML is not worth it).
- **MIME → default app**: parse `mimeapps.list` and `.desktop` `MimeType=` keys
  to build a reverse index. `launcher::app_index` already parses the files; this
  is an additional index over the same data.
- **`Exec=` with arguments**: `ExecCommand` deliberately *strips* `%f`/`%F`/
  `%u`/`%U` because a launcher never has a file to pass. We now always do — so
  it needs a sibling constructor that **substitutes** them. Small change, but it
  must not regress the launcher's no-argument behaviour.

This belongs in its own crate (`crates/mime`), is pure logic, has zero Wayland
dependency, and is fully unit-testable — a great parallel workstream.

### 3.5 File operations are deceptively hard to do *correctly*
`std::fs` gives us `copy`/`rename`/`remove`. A real file manager needs:
- **Cross-filesystem move** = copy-then-delete, not `rename` (which returns
  `EXDEV`), with rollback if it fails midway
- **Recursive copy** preserving permissions, timestamps, symlinks (copy the
  link, not the target), sparse files, hardlink identity within a tree
- **Conflict resolution**: skip / overwrite / rename-with-suffix, "apply to all"
- **Progress + cancellation** — which means operations run **off the UI thread**
  and report back over a channel, not synchronously
- **Trash spec** correctness: the `info` file with `Path=` and `DeletionDate=`,
  URL-encoding, and the top-level `.Trash-$uid` fallback for other filesystems
- Not following symlinks into recursive delete (a classic data-loss bug)

This deserves its own crate (`crates/fileops`) with an aggressive test suite
against temp dirs. **This is where data loss lives** — it should be the most
paranoid, best-tested code in the project.

---

## 4. Proposed crate/bin layout

```
crates/
  fsmodel/     directory reads, entry metadata, sorting, filtering,
               inotify-backed live updates. No UI. Pure + testable.
  fileops/     copy/move/delete/trash, conflict policy, progress reporting,
               cancellation. No UI. Paranoid tests.
  mime/        globs2 + magic sniffing, mimeapps.list, MIME→app reverse index.
  widget/      scrollable list, text input, button, menu, modal, scrollbar.
               Draws via platform::draw::Canvas.
bins/
  files/       the browser: window, view state, keybinds, layout, theming.
```

`platform::toplevel::Toplevel` (§3.1) lands in the existing `crates/platform`.

Rationale: `fsmodel`/`fileops`/`mime` are headless and testable without a
compositor — matching how `crates/launcher` is split from `bins/launcher`. This
also means the eventual portal `FileChooser` can reuse *all four* crates.

---

## 5. Suggested order (each step builds + tests + clippy before the next)

**Phase A — headless, parallelisable, zero Wayland risk**
1. `crates/fsmodel` — read a dir into sorted, filtered entries. Tests.
2. `crates/mime` — globs2 parse + extension lookup. Tests.
3. `crates/fileops` — trash first (self-contained + high value), then
   copy/move with conflict policy. Heavy tests.
4. `mime`: `mimeapps.list` + MIME→app index; `ExecCommand::for_file`.

**Phase B — the window (highest risk, do it early enough to learn)**
5. `platform::toplevel::Toplevel` — generic xdg-shell toplevel. Prove it with a
   trivial "fill the window with a colour, handle resize/close" example.

**Phase C — widgets**
6. `crates/widget`: scrollable virtualised list (the one that matters), then
   text input, then menu/modal.

**Phase D — the bin**
7. `bins/files` Tier 1: list, navigate, sort, select, open.
8. Wire `inotify` for live refresh.
9. Tier 2: operations + trash + progress UI + undo.
10. Built-in icon set (§3.3a); icon grid view.
11. Tier 3 as appetite allows.

Steps 1–4 have **no dependency on 5–6** and can proceed immediately and
independently. Step 5 is the schedule risk; starting it in parallel with Phase A
is wise.

---

## 6. Decisions (locked 2026-07-28)

1. **Icons: built-in set.** No SVG rasteriser, no system icon theme for v1.
   Styled to match this workspace's other bins.
2. **`TermWindow` convergence: later — but it *is* happening.** Build
   `platform::toplevel::Toplevel` fresh and leave the terminal alone for now,
   then **come back and converge `TermWindow` onto it** so the workspace stays
   DRY. This is a deliberate, tracked follow-up, *not* a permanent fork — so
   `Toplevel` must be designed as something `TermWindow` could plausibly sit on
   (no file-browser-specific assumptions baked into its API).
3. **List view ships first, grid is the goal.** The view mode is therefore an
   **abstraction from day one** — entry layout/hit-testing goes behind a seam so
   grid slots in later without rewriting selection, scrolling, or navigation.
   Do not hardcode "one row per entry" through the bin.
4. **Conventional keybinds for v1.** Arrows, Enter, Backspace, Ctrl+C/X/V,
   F2 rename, Delete → trash. Not vim-modal.
5. **Many windows.** No singleton; do *not* reuse `launcher::singleton`.
6. **Name: `files`** (`bins/files`).

---

## 7. Honest assessment of size

Tier 1 alone is a real project — the window and the scrollable list are each
substantial, and neither exists today. But unlike the portal, **nothing here is
blocked on protocol infrastructure we don't have**, every piece is independently
testable, and the first four steps can start today with no Wayland involvement
at all.

The two things most likely to hurt: **`Toplevel` (§3.1)** and **icon coverage
(§3.3)**. Both have a cheap escape hatch (build fresh / ship our own icons), and
I'd take both escape hatches for v1.
