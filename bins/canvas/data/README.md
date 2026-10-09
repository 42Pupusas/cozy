# The `canvas` desktop entry

`cozy.canvas.desktop.in` is the XDG desktop entry that makes `canvas`
appear in an application menu. `scripts/install.sh` substitutes `@EXEC@`
for the absolute path of the installed binary and writes the result to
`$XDG_DATA_HOME/applications/cozy.canvas.desktop`.

## Why the filename is `cozy.canvas.desktop`

The basename matches the Wayland `app_id` the binary sets
(`bins/canvas/src/main.rs` passes `"cozy.canvas"` to
`Toplevel::new_with_format`). A compositor or taskbar uses that name to
map a *window* back to the *entry* that describes it. A mismatched
basename still launches, but the window shows up as an unknown
application.

## Why `@EXEC@` and not a bare `canvas`

`Exec=` must be an absolute path. The launcher spawns entries without a
shell, and a niri session does not necessarily have `$CARGO_HOME/bin` on
`PATH`.

## `%f` and the absent `MimeType`

`canvas` takes one optional positional path: a `.json` document saved by
a previous session. `%f` is the single-file field code, so opening a
document passes that document's path.

There is deliberately no `MimeType=` line. The document is plain
`application/json`, and claiming that type would make `canvas` a
candidate handler for *every* JSON file on the system. Launching from a
menu passes no argument at all, so the entry opens the default document.

## The icon may not render in our own launcher

`Icon=applications-graphics` is the standard name for this category.
`launcher::icon_lookup::IconLookup` searches `hicolor` and
`/usr/share/pixmaps` only, PNG only, so the entry may list with no icon
depending on the installed themes. The entry is correct; the lookup is
narrow. See `bins/files/data/README.md` for the same note.
