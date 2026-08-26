# The `files` desktop entry

`cozy.files.desktop.in` is the XDG desktop entry that makes `files`
appear in an application menu. `scripts/install.sh` substitutes `@EXEC@`
for the absolute path of the installed binary and writes the result to
`$XDG_DATA_HOME/applications/cozy.files.desktop`.

## Why the filename is `cozy.files.desktop`

The basename matches the Wayland `app_id` the binary sets
(`bins/files/src/main.rs` passes `"cozy.files"` to
`Toplevel::new_with_format`). That is the convention a compositor or
taskbar uses to map a *window* back to the *entry* that describes it. A
mismatched basename still launches, but the window shows up as an
unknown application.

## Why `@EXEC@` and not a bare `files`

`Exec=` must be an absolute path. The launcher spawns entries without a
shell, and a niri session does not necessarily have `$CARGO_HOME/bin` on
`PATH` -- every other reference to these binaries in
`~/.config/niri/config.kdl` is absolute for the same reason.

## `%f` and `MimeType=inode/directory`

`files` takes one optional positional directory. `%f` is the
single-file field code, so a file manager asked to open a folder
receives that folder's path.

`MimeType=inode/directory` registers the entry as a handler for
directories, which is what lets `crates/opener` resolve "open this
folder" to this binary. Launching from a *menu* passes no argument at
all (`launcher::FieldValues::None` drops value-taking codes), so the
entry still opens the current directory when picked by name.

## The icon does not render in our own launcher

`Icon=system-file-manager` is the standard name for this category, and
it is what other desktop tools use. On this machine it resolves only
under `/usr/share/icons/AdwaitaLegacy/`, and
`launcher::icon_lookup::IconLookup` searches `hicolor` and
`/usr/share/pixmaps` only, PNG only (see `docs/filebrowser-scoping.md`
section 3.3). So `bins/launcher` lists the entry with no icon.

The entry is correct; the lookup is narrow. Widening `IconLookup` to
follow theme inheritance is a change to a shared crate, so it is
deliberately not done here.
