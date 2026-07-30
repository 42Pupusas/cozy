# Installing the `tijaax` portal backend

This directory ships the on-disk artifacts `xdg-desktop-portal` and
`systemd --user` need to discover and activate `portal` (the `tijaax`
binary) as a real `org.freedesktop.impl.portal.*` backend, per
`docs/portal-scoping.md` step 5. None of this happens automatically --
routing a new backend is a deliberate, reviewable, one-line config change
per that doc's section 4 rollback contract, not something `cargo install`
should do silently.

## Files

- **`tijaax.portal`** -- tells `xdg-desktop-portal` which D-Bus name and
  interfaces this backend implements. Install to
  `$XDG_DATA_HOME/xdg-desktop-portal/portals/tijaax.portal` (no root
  needed) or `/usr/share/xdg-desktop-portal/portals/tijaax.portal`
  (matches every other backend already on this machine, e.g.
  `xdg-desktop-portal-gtk`'s own `gtk.portal`).
- **`org.freedesktop.impl.portal.desktop.tijaax.service.in`** -- a D-Bus
  activation file; substitute `@EXEC@` for the absolute path to the
  installed `portal` binary (`scripts/install.sh` installs to
  `$CARGO_HOME/bin/portal`) and drop the result, renamed to
  `org.freedesktop.impl.portal.desktop.tijaax.service` (no `.in`), into
  `/usr/share/dbus-1/services/`.
- **`tijaax-portal.service.in`** -- likewise, a systemd user unit;
  substitute `@EXEC@` and install to
  `~/.config/systemd/user/tijaax-portal.service` or
  `/usr/lib/systemd/user/tijaax-portal.service`, matching
  `xdg-desktop-portal-gtk.service`'s own shape exactly.

## Routing it

Neither file above makes `xdg-desktop-portal` actually *use* this backend
for `Settings` -- by itself it just becomes one more installed, inactive
option. Add (or extend) a `portals.conf`:

```ini
# ~/.config/xdg-desktop-portal/portals.conf
[preferred]
org.freedesktop.impl.portal.Settings=tijaax
```

This leaves every other interface (`FileChooser`, `Notification`, ...)
routed exactly as before -- only `Settings` moves to `tijaax`. Reverting
is deleting that one line and restarting `xdg-desktop-portal.service`
(`systemctl --user restart xdg-desktop-portal.service`), no code changes
or reinstall required, per the doc's own rollback contract.
