// Orca plasmoid — the wide panel element (dolphin + framed running/open
// counter) with a native, panel-anchored popup listing the agent sessions.
// State streams from the daemon over a loopback WebSocket (see src/ws.rs);
// user actions go back as {"type":"action",...} messages.

import QtQuick
import QtQuick.Layouts
import QtWebSockets
import org.kde.plasma.plasmoid
import org.kde.plasma.components as PC3
import org.kde.kirigami as Kirigami

PlasmoidItem {
    id: root

    property var appState: ({ rows: [], running: 0, open: 0,
                              prefs: { enabled: true, on_waiting: true, on_done: true, on_error: true, sound: true } })
    readonly property bool attention: (appState.rows || []).some(r => r.status === "waiting" || r.status === "error")
    readonly property bool connected: socket.status === WebSocket.Open
    readonly property color accent: attention ? "#FF9F0A" : Kirigami.Theme.textColor

    // The daemon binds the first free port in this fixed range.
    readonly property var ports: [41957, 41958, 41959, 41960, 41961, 41962, 41963, 41964, 41965, 41966]
    property int portIndex: 0

    property bool showSettings: false
    property double nowEpoch: Date.now() / 1000

    toolTipMainText: "Orca"
    toolTipSubText: connected
        ? appState.running + " active · " + appState.open + " open"
        : i18n("daemon not running")

    switchWidth: Kirigami.Units.gridUnit * 15
    switchHeight: Kirigami.Units.gridUnit * 10

    WebSocket {
        id: socket
        url: "ws://127.0.0.1:" + root.ports[root.portIndex]
        active: true
        onTextMessageReceived: message => { root.appState = JSON.parse(message) }
    }

    Timer {
        // Declarative reconnect: runs whenever the socket is not open, so a
        // connection stuck in the Error state can never stall the retry.
        id: retry
        interval: 2000
        repeat: true
        running: socket.status !== WebSocket.Open
        onTriggered: {
            root.portIndex = (root.portIndex + 1) % root.ports.length
            socket.active = false
            socket.active = true
        }
    }

    Timer {
        // Durations tick locally; the daemon only pushes real state changes.
        interval: 1000
        running: root.expanded && (root.appState.rows || []).some(r => r.status === "running")
        repeat: true
        onTriggered: root.nowEpoch = Date.now() / 1000
    }

    function act(payload) {
        if (socket.status === WebSocket.Open) {
            socket.sendTextMessage(JSON.stringify(Object.assign({ type: "action" }, payload)))
        }
    }

    function statusColor(status) {
        switch (status) {
        case "running": return "#0A84FF"
        case "waiting": return "#FF9F0A"
        case "done": return "#30D958"
        case "error": return "#FF453A"
        default: return Kirigami.Theme.disabledTextColor
        }
    }

    function statusLabel(status) {
        return status === "waiting" ? i18n("waiting for input") : status
    }

    function formatDuration(row) {
        var seconds = row.status === "running" && row.run_started_at
            ? Math.max(0, root.nowEpoch - row.run_started_at)
            : (row.last_run_duration || 0)
        var total = Math.floor(seconds)
        if (total < 60) return total + "s"
        var minutes = Math.floor(total / 60)
        var secs = total % 60
        if (minutes < 60) return secs === 0 ? minutes + "m" : minutes + "m " + secs + "s"
        var hours = Math.floor(minutes / 60)
        var mins = minutes % 60
        return mins === 0 ? hours + "h" : hours + "h " + mins + "m"
    }

    // ------------------------------------------------------------- compact
    compactRepresentation: MouseArea {
        id: compact
        readonly property real iconSize: Math.min(height, Kirigami.Units.iconSizes.medium)
        // Breathing room: margins around the pair, clear gap between the two.
        // Panels size compact representations from the Layout attached
        // properties, not implicitWidth — set both.
        implicitWidth: compactRow.implicitWidth + Kirigami.Units.largeSpacing * 2
        Layout.minimumWidth: implicitWidth
        Layout.preferredWidth: implicitWidth
        onClicked: root.expanded = !root.expanded

        RowLayout {
            id: compactRow
            anchors.centerIn: parent
            spacing: Kirigami.Units.largeSpacing

            Image {
                source: Qt.resolvedUrl("../images/orca.png")
                sourceSize: Qt.size(compact.iconSize, compact.iconSize)
                Layout.preferredWidth: compact.iconSize
                Layout.preferredHeight: compact.iconSize
                opacity: root.connected ? 1.0 : 0.4
            }

            Rectangle {
                visible: root.connected && root.appState.open > 0
                color: "transparent"
                border.color: root.accent
                border.width: 1.4
                radius: height / 3
                // Implicit (not Layout.preferred) sizes: RowLayout's implicit
                // width sums these, and the panel allocates from that — with
                // preferred-only sizing the capsule overflowed the panel edge.
                implicitHeight: counterLabel.implicitHeight + Kirigami.Units.smallSpacing
                implicitWidth: counterLabel.implicitWidth + Kirigami.Units.smallSpacing * 2.5

                PC3.Label {
                    id: counterLabel
                    anchors.centerIn: parent
                    text: root.appState.running + "/" + root.appState.open
                    color: root.accent
                    font.bold: true
                    font.pointSize: Kirigami.Theme.smallFont.pointSize
                }
            }
        }
    }

    // ---------------------------------------------------------------- full
    fullRepresentation: ColumnLayout {
        Layout.preferredWidth: Kirigami.Units.gridUnit * 20
        Layout.preferredHeight: Kirigami.Units.gridUnit * 22
        Layout.minimumWidth: Kirigami.Units.gridUnit * 16
        spacing: Kirigami.Units.smallSpacing

        RowLayout {
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing

            Kirigami.Heading {
                level: 3
                text: "Orca"
            }
            Item { Layout.fillWidth: true }
            PC3.Label {
                text: root.appState.running + " active · " + root.appState.open + " open"
                opacity: 0.6
            }
            PC3.ToolButton {
                icon.name: "configure"
                checkable: true
                checked: root.showSettings
                onToggled: root.showSettings = checked
                PC3.ToolTip.text: i18n("Notification & sound settings")
                PC3.ToolTip.visible: hovered
            }
        }

        Kirigami.Separator { Layout.fillWidth: true }

        // Agent list
        PC3.ScrollView {
            visible: !root.showSettings
            Layout.fillWidth: true
            Layout.fillHeight: true

            ListView {
                id: list
                model: root.connected ? (root.appState.rows || []) : []
                spacing: Kirigami.Units.smallSpacing
                clip: true

                delegate: Item {
                    required property var modelData
                    width: list.width
                    height: rowContent.implicitHeight + Kirigami.Units.smallSpacing * 2

                    Rectangle {
                        anchors.fill: parent
                        radius: Kirigami.Units.cornerRadius
                        color: rowMouse.containsMouse
                            ? Qt.alpha(Kirigami.Theme.highlightColor, 0.15)
                            : "transparent"
                    }

                    MouseArea {
                        id: rowMouse
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            root.act({ action: "focus", id: modelData.id })
                            root.expanded = false
                        }
                    }

                    RowLayout {
                        id: rowContent
                        anchors.fill: parent
                        anchors.margins: Kirigami.Units.smallSpacing
                        spacing: Kirigami.Units.smallSpacing

                        Rectangle {
                            width: 9; height: 9; radius: 4.5
                            color: root.statusColor(modelData.status)
                            Layout.alignment: Qt.AlignTop
                            Layout.topMargin: 4
                        }

                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 1

                            RowLayout {
                                Layout.fillWidth: true
                                PC3.Label {
                                    Layout.fillWidth: true
                                    text: modelData.title
                                    font.weight: Font.DemiBold
                                    elide: Text.ElideRight
                                }
                                PC3.Label {
                                    text: root.formatDuration(modelData)
                                    opacity: 0.6
                                    font.family: "monospace"
                                    font.pointSize: Kirigami.Theme.smallFont.pointSize
                                }
                            }
                            PC3.Label {
                                text: modelData.source + " · " + root.statusLabel(modelData.status)
                                opacity: 0.6
                                font.pointSize: Kirigami.Theme.smallFont.pointSize
                            }
                            PC3.Label {
                                visible: !!modelData.message
                                Layout.fillWidth: true
                                text: modelData.message || ""
                                opacity: 0.5
                                elide: Text.ElideRight
                                font.pointSize: Kirigami.Theme.smallFont.pointSize
                            }
                        }

                        PC3.ToolButton {
                            icon.name: "window-close-symbolic"
                            Layout.alignment: Qt.AlignVCenter
                            onClicked: root.act({ action: "dismiss", id: modelData.id })
                            PC3.ToolTip.text: i18n("Dismiss")
                            PC3.ToolTip.visible: hovered
                        }
                    }
                }

                Kirigami.PlaceholderMessage {
                    anchors.centerIn: parent
                    width: parent.width - Kirigami.Units.gridUnit * 2
                    visible: list.count === 0
                    icon.name: root.connected ? "checkmark-symbolic" : "network-disconnect-symbolic"
                    text: root.connected ? i18n("No active agents") : i18n("Orca daemon is not running")
                    explanation: root.connected ? "" : i18n("Start it with: systemctl --user start orca")
                }
            }
        }

        // Settings pane
        ColumnLayout {
            visible: root.showSettings
            Layout.fillWidth: true
            Layout.fillHeight: true
            Layout.margins: Kirigami.Units.smallSpacing
            spacing: Kirigami.Units.smallSpacing

            Kirigami.Heading { level: 5; text: i18n("Notifications") }
            PC3.Switch {
                text: i18n("Enable notifications")
                checked: root.appState.prefs.enabled
                onToggled: root.act({ action: "set_pref", key: "enabled", value: checked })
            }
            ColumnLayout {
                Layout.leftMargin: Kirigami.Units.gridUnit
                enabled: root.appState.prefs.enabled
                PC3.Switch {
                    text: i18n("Waiting for input")
                    checked: root.appState.prefs.on_waiting
                    onToggled: root.act({ action: "set_pref", key: "on_waiting", value: checked })
                }
                PC3.Switch {
                    text: i18n("Finished")
                    checked: root.appState.prefs.on_done
                    onToggled: root.act({ action: "set_pref", key: "on_done", value: checked })
                }
                PC3.Switch {
                    text: i18n("Errors")
                    checked: root.appState.prefs.on_error
                    onToggled: root.act({ action: "set_pref", key: "on_error", value: checked })
                }
            }
            Kirigami.Heading { level: 5; text: i18n("Sound") }
            PC3.Switch {
                text: i18n("Play sound")
                enabled: root.appState.prefs.enabled
                checked: root.appState.prefs.sound
                onToggled: root.act({ action: "set_pref", key: "sound", value: checked })
            }

            Item { Layout.fillHeight: true }
        }
    }
}
