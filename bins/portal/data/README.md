# Installing the `cozy` portal backend

This directory ships the on-disk artifacts `xdg-desktop-portal` and
`systemd --user` need to discover and activate `portal` (the `cozy`
binary) as a real `org.freedesktop.impl.portal.*` backend, per
`docs/portal-scoping.md` step 5. None of this happens automatically --
routing a new backend is a deliberate, reviewable, one-line config change
per that doc's section 4 rollback contract, not something `cargo install`
should do silently.

## Files

- **`cozy.portal`** -- tells `xdg-desktop-portal` which D-Bus name and
  interfaces this backend implements. Install to
  `$XDG_DATA_HOME/xdg-desktop-portal/portals/cozy.portal` (no root
  needed) or `/usr/share/xdg-desktop-portal/portals/cozy.portal`
  (matches every other backend already on this machine, e.g.
  `xdg-desktop-portal-gtk`'s own `gtk.portal`).
- **`org.freedesktop.impl.portal.desktop.cozy.service.in`** -- a D-Bus
  activation file; substitute `@EXEC@` for the absolute path to the
  installed `portal` binary (`scripts/install.sh` installs to
  `$CARGO_HOME/bin/portal`) and drop the result, renamed to
  `org.freedesktop.impl.portal.desktop.cozy.service` (no `.in`), into
  `/usr/share/dbus-1/services/`.
- **`cozy-portal.service.in`** -- likewise, a systemd user unit;
  substitute `@EXEC@` and install to
  `~/.config/systemd/user/cozy-portal.service` or
  `/usr/lib/systemd/user/cozy-portal.service`, matching
  `xdg-desktop-portal-gtk.service`'s own shape exactly.

## Routing it

Neither file above makes `xdg-desktop-portal` actually *use* this backend
for `Settings` -- by itself it just becomes one more installed, inactive
option. Add (or extend) a `portals.conf`:

```ini
# ~/.config/xdg-desktop-portal/portals.conf
[preferred]
org.freedesktop.impl.portal.Settings=cozy
```

This leaves every other interface (`FileChooser`, `Notification`, ...)
routed exactly as before -- only `Settings` moves to `cozy`. Reverting
is deleting that one line and restarting `xdg-desktop-portal.service`
(`systemctl --user restart xdg-desktop-portal.service`), no code changes
or reinstall required, per the doc's own rollback contract.

## Renaming note

This backend was previously named `tijaax`. If you already installed the
old files, remove them, or two backends compete for the same interface:

- `$XDG_DATA_HOME/xdg-desktop-portal/portals/tijaax.portal`
- `/usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.tijaax.service`
- `~/.config/systemd/user/tijaax-portal.service`

Then change your `portals.conf` line from `=tijaax` to `=cozy` and run
`systemctl --user restart xdg-desktop-portal.service`.
