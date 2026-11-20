# Sealwax bar widget for Omarchy

An Omarchy bar widget that shows Sealwax's icon with the number of unread messages in your
Inboxes, the folders under them and your own folders (not Sent, Drafts, Archive, Junk or Trash).
At 0 the icon shows alone.

- **Click** the icon to see where the unread mail is: each folder with unread mail and how many,
  under its account's name when you have more than one account. Click a folder to open Sealwax
  on it, or **Open Sealwax** at the bottom.
- **Right-click** the icon to open Sealwax. While Sealwax isn't running, a click opens it.

It needs Sealwax 1.5.0 or later; the list of folders needs Sealwax 1.6.3 (with an older Sealwax
each account shows one line that opens Sealwax, and the number counts only the Inboxes). While Sealwax runs, with or without a window, it keeps the
number in `$XDG_RUNTIME_DIR/sealwax/unread.json`; the widget reads only that file, never your
mail, and Sealwax removes it when it quits. `sealwax --unread` prints the same number for
scripts and other bars (it exits 1 when Sealwax isn't running).

## Install

In a terminal, this line asks where in the bar the widget goes (left, center beside the clock,
or right; center is the default), downloads it into
`~/.config/omarchy/plugins/io.github.pixdevsapps.sealwax-unread/` and turns it on there:

```sh
s=$(gum choose --header "Where in the bar?" --selected center left center right) && d=~/.config/omarchy/plugins/io.github.pixdevsapps.sealwax-unread && mkdir -p "$d" && for f in manifest.json BarWidget.qml; do curl -fsSL -o "$d/$f" https://raw.githubusercontent.com/PixDevsApps/sealwax/main/contrib/omarchy-plugin/$f; done && omarchy-shell shell rescanPlugins && { for i in {1..50}; do omarchy plugin list --json | grep -q '"io.github.pixdevsapps.sealwax-unread"' && break; sleep 0.1; done; omarchy plugin enable io.github.pixdevsapps.sealwax-unread --section "${s/center/centerEnd}"; }
```

From a checkout, copying this folder to that path does the same. Run the line again to update;
`omarchy plugin remove io.github.pixdevsapps.sealwax-unread` takes it off.

## Settings

In the widget's entry in `shell.json`:

| Key | Default | What it does |
| --- | --- | --- |
| `flatpak` | `false` | `true` when Sealwax is installed as a Flatpak: the count is read from `$XDG_RUNTIME_DIR/app/io.github.pixdevsapps.Sealwax/unread.json` and a click runs `flatpak run io.github.pixdevsapps.Sealwax` |
| `command` | `uwsm-app -- sealwax` | What a click runs; a folder's line adds `--folder KEY` |

## The file

```json
{"version":1,"pid":4242,"unread":12,"accounts":[
  {"name":"Work","unread":9,"folders":[
    {"key":"folder-3","name":"Inbox","parent":"","unread":7},
    {"key":"folder-9","name":"Bank","parent":"Inbox","unread":2}]},
  {"name":"Account 2","unread":3,"folders":[
    {"key":"folder-21","name":"Inbox","parent":"","unread":3}]}]}
```

`unread` is the total; each account in `accounts` is named by its label (an account without a
label is "Account 2", never its address), with its number and `folders`: each of its counted
folders that has unread mail, in the sidebar's order, by the name the sidebar shows and the names
of the folders it is under (`parent`, apart by `/`). `key` is what `sealwax --folder` opens: always
`folder-` and a number, or `all-inboxes`. A reader should skip a file whose `version` it doesn't
know; fields may be added within a version (`folders` came in Sealwax 1.6.3), so a reader should
ignore the ones it doesn't know.
