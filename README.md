# Ice Commander plugin SDK

Worked examples of writing a plugin for Ice Commander. Each is one crate, each
shows one thing, and each is small enough to read in a sitting.

A plugin is a shared library exporting a handful of C functions. It never draws
anything: it describes what it offers as JSON, and whichever frontend is running
builds that — the desktop application, a terminal, or a browser. The same library
file therefore works in all three.

An example depends on one crate and nothing else: `ic-plugin-api`, the contract,
taken from [github.com/ice-commander/plugin-api](https://github.com/ice-commander/plugin-api).

A plugin that has nothing to offer in this application or on this system — a picture
viewer in a terminal, a Windows registry editor on macOS — answers `IC_ERR_NOT_THIS_HOST`
from `init` before registering anything, and the application skips it quietly.

## The examples

**`hello-toolbar`** — a button on the panel toolbar that opens a window described
in JSON. The smallest thing that puts something of your own on the screen.

**`hello-header`** — the same window, opened from a button on the window header
instead, and the button changes its picture on every press.

**`hello-events`** — a window that reads what is typed into it, reacts as it is
typed, and answers button presses.

**`hello-fs`** — a file extension that opens like a folder. The smallest
filesystem there is: it lists, it reads, and nothing else.

**`hello-panel`** — the same idea carried as far as it goes. Open a `.checklist`
and every line becomes an entry with two columns the plugin put there, one of
them a tick box you can click; the panel's usual buttons give way to four of its
own, and a count appears in the window header while anything is left.

**`hello-drive`** — entries of a plugin's own beside the disks, each remembering
how often it was opened, plus a pinned entry in the connections list that opens a
page about them. The shape for a share on another machine, a device, a service.

**`hello-view`** — one file shown three ways. The plugin is told which frontend it
was loaded into and answers with a different document for each.

**`website-fs`** — a connection kind: the settings form the host renders for it,
and the filesystem behind it once the form is filled in.

**`hello-tree`** — a window with an interface of its own, shaped like a registry
editor: a tree whose branches are filled in as they are opened, and a table that
follows the selection. Delete and F5 work with no button on screen, and one branch
keeps changing on its own through `view_invalidate`.

**`hello-source`** — a panel of the plugin's own that is not a filesystem: a list of
tasks with columns of its own, opened from a toolbar button, with actions on the
selected rows. The same setup as a process list.

**`hello-canvas`** — a viewer that paints its own picture rather than describing
one: a `canvas` node, GL looked up through the host in `canvas_ready`, a frame in
`canvas_draw`, and `canvas_invalidate` for the next one. Only the desktop
application hands out GL, so elsewhere it declines.

**`hello-media`** — a viewer that plays a `.wav` without decoding it: a `media` node
names the file and the application plays it. When the track ends the application
sends `ended`, and the plugin moves on to the next file in the folder.

**`hello-shell`** — a connection kind for a folder on this machine whose panel has
a terminal and offers chmod. The terminal is a line-echo shell the plugin answers
itself, so it starts no process and works the same everywhere.

## Building

```sh
git clone git@github.com:ice-commander/plugin-sdk.git
./build.sh          # every example into bin/
./test.sh           # the whole workspace
./check.sh          # loads each built library and says what it registered (IC_PLUGIN_CHECK)
./deploy-local.sh   # copies bin/ where the application looks for plugins
```

## Where the application looks

A plugin is loaded from one folder, and only the enabled ones are opened at all —
the application's settings page is where they are switched on.

| system | folder | library |
|---|---|---|
| Linux | `${XDG_DATA_HOME:-$HOME/.local/share}/ice-commander/plugins` | `libsdk_hello_fs.so` |
| macOS | `$HOME/Library/Application Support/ice-commander/plugins` | `libsdk_hello_fs.dylib` |
| Windows | `%APPDATA%\ice-commander\plugins` | `sdk_hello_fs.dll` |

`IC_PLUGIN_DIR` overrides it, for both the application and `deploy-local.sh` —
which is how to try a plugin without touching the folder a real install uses.

Rust stable, edition 2021. A C toolchain, which a few transitive dependencies
build with. No GTK: an example links against none of the application's interface.

`check.sh` needs the plugin checker: set `IC_PLUGIN_CHECK` to a built `ic-plugin-check`.

## Licence

MIT or Apache-2.0, at your option, except for the icons listed in
`THIRD-PARTY-LICENSES.md`. Contributions carry a `Signed-off-by` line, per the
Developer Certificate of Origin in `DCO`.
