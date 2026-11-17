# Sealwax bar widget for Omarchy

An Omarchy bar widget that shows Sealwax's icon with the number of unread messages in your
Inboxes (All Inboxes' number), and opens Sealwax when you click it. At 0 the icon shows alone.

It needs Sealwax 1.5.0 or later. While Sealwax runs, with or without a window, it keeps the
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
| `command` | `uwsm-app -- sealwax` | What a click runs |

## The file

```json
{"version":1,"pid":4242,"unread":12,"accounts":[{"name":"Work","unread":9},{"name":"Account 2","unread":3}]}
```

`unread` is the total; `accounts` gives each account's Inbox by its label (an account without a
label is "Account 2", never its address). A reader should skip a file whose `version` it
doesn't know.
