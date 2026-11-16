import QtQuick
import Quickshell
import Quickshell.Io
import qs.Ui

// Sealwax in the bar (issue #17): a mail icon with the number of unread messages in the
// Inboxes, nothing beside it at 0, and a click that opens Sealwax. The number is the one Sealwax
// keeps in $XDG_RUNTIME_DIR/sealwax/unread.json while it runs (in the Flatpak's folder with
// "flatpak": true in this widget's shell.json entry); the widget never reads the mail store.
// Without the file Sealwax isn't running, and the icon shows alone.
BarWidget {
  id: root
  moduleName: "io.github.pixdevsapps.sealwax-unread"

  property int unread: 0
  property bool running: false

  readonly property bool flatpak: setting("flatpak", false) === true
  readonly property string countPath: Quickshell.env("XDG_RUNTIME_DIR")
    + (flatpak ? "/app/io.github.pixdevsapps.Sealwax/unread.json" : "/sealwax/unread.json")
  readonly property string launch: setting("command",
    flatpak ? "flatpak run io.github.pixdevsapps.Sealwax" : "uwsm-app -- sealwax")

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  function read(text) {
    try {
      var count = JSON.parse(text)
      if (count.version !== 1 || typeof count.unread !== "number") throw "unknown format"
      root.unread = Math.max(0, Math.floor(count.unread))
      root.running = true
    } catch (error) {
      root.gone()
    }
  }

  function gone() {
    root.unread = 0
    root.running = false
  }

  FileView {
    id: countFile
    path: root.countPath
    watchChanges: true
    printErrors: false
    onLoaded: root.read(text())
    onLoadFailed: root.gone()
    onFileChanged: reload()
  }

  // Sealwax replaces the file rather than writing into it, and removes it when it quits; a look
  // every few seconds catches what a file watch can miss across those.
  Timer {
    interval: 5000
    running: true
    repeat: true
    onTriggered: countFile.reload()
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: "󰇮"
    labelText: root.unread > 0 ? String(root.unread) : ""
    active: root.unread > 0
    tooltipText: !root.running ? "Sealwax isn't running; click to open it"
      : root.unread === 1 ? "1 unread message"
      : root.unread > 0 ? root.unread + " unread messages"
      : "No unread mail"
    onPressed: function(b) {
      if (root.bar) root.bar.run(root.launch)
    }
  }
}
