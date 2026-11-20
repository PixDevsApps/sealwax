import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// Sealwax in the bar (issue #17): a mail icon with the number of unread messages, nothing beside
// it at 0. The number is the one Sealwax keeps in $XDG_RUNTIME_DIR/sealwax/unread.json while it
// runs (in the Flatpak's folder with "flatpak": true in this widget's shell.json entry): the
// Inboxes, the folders under them and the user's own folders. A click lists the folders with
// unread mail, by account, and a click on one opens Sealwax on it (`sealwax --folder KEY`); a
// right click opens Sealwax. The widget never reads the mail store. Without the file Sealwax
// isn't running: the icon shows alone and a click opens Sealwax.
BarWidget {
  id: root
  moduleName: "io.github.pixdevsapps.sealwax-unread"

  property int unread: 0
  property bool running: false
  // [{ name, unread, folders: [{ key, name, parent, unread }] }], only the accounts and folders
  // with unread mail.
  property var accounts: []
  // More than one account in the file: the list names them.
  property bool severalAccounts: false
  property bool popupOpen: false

  readonly property color foreground: bar ? bar.foreground : Commons.Color.foreground
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  readonly property bool flatpak: setting("flatpak", false) === true
  readonly property string countPath: Quickshell.env("XDG_RUNTIME_DIR")
    + (flatpak ? "/app/io.github.pixdevsapps.Sealwax/unread.json" : "/sealwax/unread.json")
  readonly property string launch: setting("command",
    flatpak ? "flatpak run io.github.pixdevsapps.Sealwax" : "uwsm-app -- sealwax")

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  // What the bar calls to close the list (PopupCard's owner, another widget's popup opening).
  function close() { root.popupOpen = false }

  // Sealwax's sidebar keys; anything else in the file is left out, since a key goes on a
  // command line.
  function isKey(key) {
    return typeof key === "string" && /^(folder-[0-9]{1,19}|all-inboxes)$/.test(key)
  }

  function count(value) {
    return typeof value === "number" && isFinite(value) ? Math.max(0, Math.floor(value)) : 0
  }

  function read(text) {
    try {
      var file = JSON.parse(text)
      if (file.version !== 1 || typeof file.unread !== "number") throw "unknown format"
      var listed = Array.isArray(file.accounts) ? file.accounts : []
      var accounts = []
      for (var i = 0; i < listed.length; i++) {
        var account = listed[i]
        if (!account || count(account.unread) === 0) continue
        var folders = []
        // Sealwax 1.5.0 to 1.6.2 gave each account's Inbox count without its folders.
        var given = Array.isArray(account.folders) ? account.folders
          : [{ key: "", name: "Inbox", parent: "", unread: account.unread }]
        for (var j = 0; j < given.length; j++) {
          var folder = given[j]
          if (!folder || count(folder.unread) === 0) continue
          folders.push({
            key: isKey(folder.key) ? folder.key : "",
            name: String(folder.name || ""),
            parent: String(folder.parent || ""),
            unread: count(folder.unread)
          })
        }
        if (folders.length > 0)
          accounts.push({ name: String(account.name || ""), unread: count(account.unread), folders: folders })
      }
      root.unread = count(file.unread)
      root.accounts = accounts
      root.severalAccounts = listed.length > 1
      root.running = true
    } catch (error) {
      root.gone()
    }
  }

  function gone() {
    root.unread = 0
    root.accounts = []
    root.severalAccounts = false
    root.running = false
    root.popupOpen = false
  }

  // Sealwax, on the folder `key` when there is one.
  function openSealwax(key) {
    root.popupOpen = false
    if (!root.bar) return
    root.bar.run(isKey(key) ? root.launch + " --folder " + key : root.launch)
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
    tooltipText: root.popupOpen ? ""
      : !root.running ? "Sealwax isn't running; click to open it"
      : root.unread === 1 ? "1 unread message"
      : root.unread > 0 ? root.unread + " unread messages"
      : "No unread mail"
    onPressed: function(b) {
      if (b === Qt.RightButton || !root.running) root.openSealwax("")
      else root.popupOpen = !root.popupOpen
    }
  }

  // A line of the list: a name, a dimmer detail after it, and a number at the end.
  component ListRow: BorderSurface {
    id: row

    property color foreground: Commons.Color.foreground
    property string fontFamily: Style.font.family
    property string title: ""
    property string detail: ""
    property string number: ""
    signal activated()

    width: parent ? parent.width : 0
    height: Math.max(rowTitle.implicitHeight, rowNumber.implicitHeight) + Style.space(10)
    radius: Style.spacing.labelGap
    color: rowMouse.containsMouse ? Style.hoverFillFor(row.foreground, Commons.Color.accent) : "transparent"

    Text {
      id: rowTitle
      textFormat: Text.PlainText
      anchors.left: parent.left
      anchors.leftMargin: Style.space(8)
      anchors.verticalCenter: parent.verticalCenter
      width: Math.min(implicitWidth, parent.width - rowNumber.width - Style.space(28))
      text: row.title
      color: row.foreground
      font.family: row.fontFamily
      font.pixelSize: Style.font.body
      elide: Text.ElideRight
    }

    Text {
      textFormat: Text.PlainText
      anchors.left: rowTitle.right
      anchors.leftMargin: Style.space(6)
      anchors.right: rowNumber.left
      anchors.rightMargin: Style.space(8)
      anchors.verticalCenter: parent.verticalCenter
      text: row.detail
      visible: text !== ""
      color: Qt.darker(row.foreground, 1.6)
      font.family: row.fontFamily
      font.pixelSize: Style.font.caption
      elide: Text.ElideLeft
    }

    Text {
      id: rowNumber
      textFormat: Text.PlainText
      anchors.right: parent.right
      anchors.rightMargin: Style.space(8)
      anchors.verticalCenter: parent.verticalCenter
      text: row.number
      color: Commons.Color.accent
      font.family: row.fontFamily
      font.pixelSize: Style.font.body
      font.bold: true
    }

    MouseArea {
      id: rowMouse
      anchors.fill: parent
      hoverEnabled: true
      cursorShape: Qt.PointingHandCursor
      onClicked: row.activated()
    }
  }

  PopupCard {
    id: popup
    anchorItem: root
    bar: root.bar
    owner: root
    open: root.popupOpen
    contentWidth: popup.fittedContentWidth(Style.space(300))
    contentHeight: popup.fittedContentHeight(list.implicitHeight, Style.space(560))

    Flickable {
      anchors.fill: parent
      contentWidth: width
      contentHeight: list.implicitHeight
      interactive: contentHeight > height
      clip: interactive
      boundsBehavior: Flickable.StopAtBounds

      Column {
        id: list
        width: parent.width
        spacing: Style.space(4)

        Item {
          width: parent.width
          height: heading.implicitHeight + Style.space(4)

          Text {
            id: heading
            textFormat: Text.PlainText
            anchors.left: parent.left
            anchors.leftMargin: Style.space(8)
            text: "Sealwax"
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.subtitle
            font.bold: true
          }

          Text {
            textFormat: Text.PlainText
            anchors.right: parent.right
            anchors.rightMargin: Style.space(8)
            anchors.baseline: heading.baseline
            text: root.unread === 0 ? "No unread mail" : root.unread + " unread"
            color: Qt.darker(root.foreground, 1.4)
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
          }
        }

        Repeater {
          model: root.accounts

          Column {
            id: section
            required property var modelData
            width: list.width
            spacing: Style.space(2)

            PanelSectionHeader {
              visible: root.severalAccounts
              leftPadding: Style.space(8)
              topPadding: Style.space(6)
              text: section.modelData.name.toUpperCase()
              foreground: root.foreground
              fontFamily: root.fontFamily
            }

            Repeater {
              model: section.modelData.folders

              ListRow {
                required property var modelData
                width: section.width
                foreground: root.foreground
                fontFamily: root.fontFamily
                title: modelData.name
                detail: modelData.parent !== "" ? "in " + modelData.parent : ""
                number: String(modelData.unread)
                onActivated: root.openSealwax(modelData.key)
              }
            }
          }
        }

        PanelSeparator {
          foreground: root.foreground
        }

        ListRow {
          width: list.width
          foreground: root.foreground
          fontFamily: root.fontFamily
          title: "Open Sealwax"
          onActivated: root.openSealwax("")
        }
      }
    }
  }
}
