# Portal compatibility matrix

Step 18 of `PLAN.md`. Status values: `measured` came from this machine,
`spec` came from the installed interface XML, `unmeasured` still needs the
commands in the last section.

## Environment

| Fact | Value | Source |
| --- | --- | --- |
| Compositor | niri (`XDG_CURRENT_DESKTOP=niri`) | measured |
| Bus daemon | dbus-broker; a reload is needed after creating a new service directory | measured |
| Flatpak | not installed (`/usr/bin/flatpak` absent) | measured |
| Installed backend descriptors | `gtk.portal` only, `UseIn=gnome` | measured |
| `xdg-desktop-portal-wlr` | not installed | measured |
| `xdg-desktop-portal-gnome` | no descriptor installed | measured |
| Distro routing file | `/usr/share/xdg-desktop-portal/niri-portals.conf`: `default=gnome;gtk;`, Access and Notification to gtk, Secret to gnome-keyring | measured |

## Current routing

`~/.config/xdg-desktop-portal/portals.conf`:

```
[preferred]
default=gtk
org.freedesktop.impl.portal.Settings=cozy
org.freedesktop.impl.portal.Screenshot=wlr
org.freedesktop.impl.portal.ScreenCast=wlr
```

`Screenshot=wlr` and `ScreenCast=wlr` name a backend that has no installed
descriptor, so those two interfaces have no working provider through this
file.

## Backend interfaces

| Interface | Provider today | cozy status | Fallback | Version cozy reports |
| --- | --- | --- | --- | --- |
| `Settings` | cozy, merged with gtk | routed | remove the `Settings=cozy` line | 2 |
| `FileChooser` | gtk | not started | gtk | none |
| `AppChooser` | gtk | not started | gtk | none |
| `Access` | gtk | not started | gtk | none |
| `Account` | gtk | not started | gtk | none |
| `Email` | cozy (routed in `portals.conf`) | implemented, unmeasured against a real caller | remove the `Email=cozy` line | none |
| `Inhibit` | gtk | not started | gtk | none |
| `Print` | gtk | not started | gtk | none |
| `Notification` | gtk | not started | gtk | none |
| `Lockdown` | gtk | not started | gtk | none |
| `DynamicLauncher` | gtk | not started | gtk | none |
| `Screenshot` | none installed | not started | none | none |
| `ScreenCast` | none installed | not started | none | none |
| `Secret` | gnome-keyring per distro file, overridden by `default=gtk` | not started | unmeasured | none |

`OpenURI` is a frontend interface with no backend. `crates/opener` already
implements the launch side.

## Scope

The portal calls this desktop uses, as stated by the user: email, file
chooser, open browser and open programs, and screen sharing in video calls.
Everything else stays with gtk or with the frontend.

| Need | Interface | State |
| --- | --- | --- |
| Open browser, open programs | `OpenURI` | works through the frontend; opening `https://example.com` launched the browser and the user confirmed it works. A command-line call carries no activation token, so only app-initiated calls can move focus. |
| Email | `Email` | cozy backend implemented: `ComposeEmail` builds a `mailto:` URL and opens it through `crates/opener`; response 0 on launch, 2 when no handler or the spawn fails. Unit tests and a real-bus test pass. Not yet confirmed with Thunderbird through the frontend. |
| Find files | `FileChooser` | gtk; cozy backend not started |
| Screen sharing | `ScreenCast` | no provider installed; `xdg-desktop-portal-gnome` and `xdg-desktop-portal-wlr` are both absent |

## Frontend interfaces exposed on the bus

From `busctl --user introspect org.freedesktop.portal.Desktop`, paired by the
rule that a `.version` line belongs to the interface line above it.

| Interface | Version | Needs a backend |
| --- | --- | --- |
| `Account` | 1 | yes, gtk |
| `Camera` | 1 | no |
| `DynamicLauncher` | 1 | yes, gtk |
| `Email` | 4 | yes, gtk |
| `FileChooser` | 4 | yes, gtk |
| `GameMode` | 4 | no |
| `Inhibit` | 3 | yes, gtk |
| `Location` | 1 | no |
| `MemoryMonitor` | 1 | no |
| `NetworkMonitor` | 3 | no |
| `Notification` | 1 | yes, gtk |
| `OpenURI` | 5 | no |
| `PowerProfileMonitor` | 1 | no |
| `Print` | 4 | yes, gtk |
| `ProxyResolver` | 1 | no |
| `Realtime` | 1 | no |
| `Settings` | 2 | yes, gtk and cozy |
| `Trash` | 1 | no |

One more `.version` line (value 1) came before `Account`. Its interface line
does not start with `org.freedesktop.portal`, so the filter dropped it. It
is probably `org.freedesktop.host.portal.Registry`, which is unconfirmed.

These frontend interfaces are absent from the output: `Screenshot`,
`ScreenCast`, `Wallpaper`, `Background`, `Secret`, `Clipboard`,
`InputCapture`, `RemoteDesktop`, `GlobalShortcuts`, `Usb`. The `Screenshot`
and `ScreenCast` absence agrees with the missing `wlr` backend. The reason
for the others was not checked.

## Settings detail

| Item | Value | Source |
| --- | --- | --- |
| Methods | `Read`, `ReadAll` | spec |
| Signal | `SettingChanged` | spec |
| Property | `version` (`u`) | spec |
| Namespace served by cozy | `org.freedesktop.appearance` | measured |
| Keys served by cozy | `color-scheme`, `accent-color`, `contrast`, `reduced-motion` | measured |
| Namespaces served by gtk only | every `org.gnome.*` | measured |
| Frontend behaviour | merges all Settings backends | measured |
| Precedence on a shared key | cozy wins (`color-scheme` followed cozy to `2` while gtk reported `1`) | measured |
| Live reload | config change reaches the frontend within about 1 second | measured |
| Rollback | delete the route line, restart `xdg-desktop-portal.service`; gtk's 2-key answer returns | measured |
| Real caller | `busctl` only; no GTK or Qt application has been checked | measured |
| Options | none; `Read` and `ReadAll` take no option dictionary | spec |

## Email detail

| Item | Value | Source |
| --- | --- | --- |
| Method | `ComposeEmail(o, s, s, a{sv}) -> (u, a{sv})` | spec |
| Option keys read | `address`, `addresses`, `cc`, `bcc`, `subject`, `body`, `attachments`, `activation_token` | spec |
| Recipient order | `address` first, then `addresses` | gtk `email.c` |
| Escaping | `subject`, `body` and each attachment percent-encoded; addresses keep `@` and `+` | cozy choice |
| Empty `subject`/`body` | left out of the URL (gtk always emits them) | cozy choice |
| Handler | `x-scheme-handler/mailto` from `mimeapps.list`, currently Thunderbird | measured |
| Results | always an empty `a{sv}` | gtk `email.c` |
| Rollback | delete the `Email=cozy` line, restart `xdg-desktop-portal.service` | measured |
| Real caller | none yet | unmeasured |

Verify with:

```
gdbus call --session --dest org.freedesktop.portal.Desktop --object-path /org/freedesktop/portal/desktop --method org.freedesktop.portal.Email.ComposeEmail "" "{'address': <'you@example.com'>, 'subject': <'test'>}"
```

## Unmeasured

Run these and record the results in the tables above.

Applications that actually call a portal while in daily use:

```
busctl --user monitor org.freedesktop.portal.Desktop --json=short
```

Leave the monitor running during normal use, then list the distinct
interface and method names with their sender. Each interface that shows up
needs a row with its real caller.

Whether a toolkit reads cozy's values: start a GTK4 application, change
`[colors].background` in the terminal config, and check that the application
follows `color-scheme`.
