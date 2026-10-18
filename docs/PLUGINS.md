# Plugins

A plugin adds something to Sealwax without being part of it: a command in the message menu
and the command palette (**Create Event**, **Send to Task List**, **Save to Notes**), or a
program that runs when new mail arrives. Plugins are ordinary programs, written in any language.
Sealwax runs them and sends them the message as JSON. They never run inside Sealwax, so a
plugin can't read Sealwax's memory, its mail store, your passwords or your sign-in tokens, and a
plugin that crashes or hangs can't take Sealwax with it. [ADR-022](DECISIONS.md#adr-022--plugins-programs-sealwax-runs-with-json-2026-10-18)
explains the design.

## Installing a plugin

A plugin is a folder in `~/.config/sealwax/plugins/` holding a `plugin.toml`, and often the
plugin's program too:

```
~/.config/sealwax/plugins/
└── signet/
    └── plugin.toml
```

Sealwax reads the plugins when it starts and again as soon as anything in that folder changes,
so a plugin you add is there at once. If a `plugin.toml` has a mistake, Sealwax says so in a
toast, with the line and column, and leaves that plugin out until the file is fixed.

**Plugins run as you.** A plugin can do anything a program you start yourself can do, and it
is sent the parts of your mail its manifest asks for. Install plugins you trust, as you would
any program. Sealwax refuses a plugin whose folder or `plugin.toml` another user could change
(fix it with `chmod go-w`).

To turn a plugin off without removing it, add `enabled = false` to its `plugin.toml`.

## plugin.toml

```toml
name = "Signet"                                   # what Sealwax calls the plugin
description = "Create calendar events from email"
command = ["signet", "--from-sealwax"]            # the program and its arguments

[[action]]
id = "create-event"                               # a–z, 0–9, - and _
title = "Create Event"                            # the menu and palette text
receives = ["text"]                               # besides the headers; see below

[[hook]]
event = "new-mail"
receives = []
```

| Setting | Meaning |
| --- | --- |
| `name` | What Sealwax calls the plugin in toasts. The folder's name when left out. |
| `description` | One line about what it does. |
| `command` | The program and its arguments, as a list. A program without a `/` is looked up on your `PATH`. A relative path with a `/` (`"./run.py"`) is in the plugin's folder. The program runs in the plugin's folder. |
| `enabled` | `false` turns the plugin off. `true` by default. |
| `[[action]]` | A command in the message menu (right-click a message), after Sealwax's own, and in the command palette (F1) while messages are selected. An action runs on the selected messages, at most 50 of them, all in one account. |
| `[[hook]]` | Runs the plugin when something happens. `event = "new-mail"`: new messages in an Inbox, once per batch, after your rules have run. |
| `receives` | What of each message the plugin is sent besides its headers. `"text"` adds the body as plain text (HTML converted when there is no plain part), and `"raw"` adds the whole message as the server has it, in base64. Leave it out, or write `[]`, for headers alone. |

For an action that asks for `text` or `raw`, Sealwax downloads the message first if needs be,
as it does to show it. A hook is sent only what is already on this computer (`null` otherwise),
so a hook never makes Sealwax download mail.

## What a plugin is sent

The program gets one JSON document on standard input, then end of input:

```json
{
  "version": 1,
  "event": "action",
  "plugin": "signet",
  "action": "create-event",
  "account": {"key": "work", "address": "ada@example.com", "label": "Work"},
  "messages": [
    {
      "folder": "INBOX",
      "message_id": "m1@example.com",
      "subject": "Planning call",
      "from": [{"name": "Grace Hopper", "address": "grace@example.com"}],
      "to": [{"name": "", "address": "ada@example.com"}],
      "cc": [],
      "reply_to": [],
      "date": 1792314720,
      "text": "Let's talk Thursday.\nJoin: https://meet.example/abc\n",
      "raw": null
    }
  ]
}
```

- `version` is the protocol's version. It goes up only when a change would break existing
  plugins. New fields may appear in any version, so ignore what you don't know.
- `event` is `"action"` or the hook's event (`"new-mail"`). `action` is the action's id, or
  `null` for a hook.
- `account` is the account the messages are in: its key in `accounts.toml`, its address and
  what Sealwax calls it.
- `date` is the Date header in Unix seconds, or `null`. `message_id` has no angle brackets.
- `text` and `raw` are `null` unless the action or hook asks for them (and, for a hook, the
  message is on this computer). Lines in `text` end in `\n`.

The environment has `SEALWAX_PLUGIN` (the plugin's id) and `SEALWAX_PLUGIN_PROTOCOL` (`1`).

## What a plugin answers

Write nothing, or one JSON object, to standard output, and exit with status 0:

```json
{"notice": "Event created"}
```

`notice` is shown as a toast, at most 200 characters. Exit with any other status to say the
plugin failed: Sealwax shows the plugin's name, "failed" and the first line it wrote to standard
error, for example "Signet: failed: no calendar is set up". Output past 64 KiB is dropped.

An action has 30 seconds and a hook 10. Then Sealwax stops the plugin and everything it
started, and says so. A plugin that opens a window should hand the work to that window
(start it in the background, or pass it to a running instance) and return at once. Hooks run one
at a time, in the order the mail arrived.

## Examples

A Python plugin that saves the selected messages' subjects and senders to a file:

```toml
# ~/.config/sealwax/plugins/note/plugin.toml
name = "Note"
command = ["python3", "note.py"]

[[action]]
id = "save"
title = "Save to Notes"
```

```python
# ~/.config/sealwax/plugins/note/note.py
import json, sys, pathlib

request = json.load(sys.stdin)
notes = pathlib.Path.home() / "notes.md"
with notes.open("a") as file:
    for message in request["messages"]:
        sender = message["from"][0]["address"] if message["from"] else "unknown"
        file.write(f"- {message['subject']} ({sender})\n")
print(json.dumps({"notice": f"Saved {len(request['messages'])} to notes.md"}))
```

A hook that logs new mail's senders, with headers only:

```toml
# ~/.config/sealwax/plugins/log/plugin.toml
command = ["sh", "-c", "jq -r '.messages[].from[0].address' >> \"$HOME/new-mail.log\""]

[[hook]]
event = "new-mail"
```
