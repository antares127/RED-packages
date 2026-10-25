import Quickshell
import Quickshell.Io
import QtQuick
import QtQuick.Layouts

ShellRoot {
    id: root

    readonly property var state: {
        const raw = Quickshell.env("RED_GUI_STATE");
        try {
            return raw ? JSON.parse(raw) : ({});
        } catch (e) {
            return ({});
        }
    }

    readonly property var palette: state.theme || ({})

    readonly property string bin: Quickshell.env("RED_GUI_BIN") || "red"

    readonly property color surface: palette.surface || "#101418"
    readonly property color surfaceLow: palette.surface_low || "#191c20"
    readonly property color surfaceHigh: palette.surface_high || "#1d2024"
    readonly property color foreground: palette.foreground || "#e0e2e8"
    readonly property color foregroundSoft: palette.foreground_soft || "#c2c7cf"
    readonly property color muted: palette.muted || "#8c9199"
    readonly property color outline: palette.outline || "#8c9199"
    readonly property color outlineVariant: palette.outline_variant || "#42474e"
    readonly property color primary: palette.primary || "#42a5f5"
    readonly property color onPrimary: palette.on_primary || "#000000"
    readonly property color danger: palette.danger || "#f2b8b5"

    property bool showDetails: false
    property bool running: false
    property bool finished: false
    property bool ok: false
    property string status: ""
    property var logLines: []
    property real phase: 0
    property string activeAction: ""

    function tint(c, a) {
        return Qt.rgba(c.r, c.g, c.b, a);
    }

    function appendLog(line) {
        if (!line || line.length === 0)
            return;
        const lines = logLines.slice();
        lines.push(line);
        if (lines.length > 240)
            lines.splice(0, lines.length - 240);
        logLines = lines;
    }

    function commandFor(action) {
        const s = state;
        if (action === "install") {
            return s.root
                ? [bin, "-install", s.file, "--force"]
                : ["pkexec", bin, "-install", s.file, "--force"];
        }
        if (action === "rebuild")
            return [bin, "-rebuild", s.file];
        if (action === "system")
            return [bin, "-build", s.file, "--native", "--arch", s.arch];
        if (action === "bundle")
            return [bin, "-bundle", s.file];
        return [bin, "-build", s.file];
    }

    function start(action) {
        if (running)
            return;

        activeAction = action;
        logLines = [];
        finished = false;
        ok = false;
        status = "正在执行…";
        running = true;

        runner.command = commandFor(action);
        runner.running = true;
    }

    function finish(code) {
        running = false;
        finished = true;
        ok = code === 0;
        activeAction = "";

        const text = logLines.join("\n");
        const lines = text.split("\n");
        const created = lines.find(line => line.indexOf("==> Created ") === 0);
        const bundled = lines.find(line => line.indexOf("==> Installed bundle ") === 0);
        if (ok) {
            if (created)
                status = "已生成 " + created.replace("==> Created ", "").split(" (")[0];
            else if (bundled)
                status = bundled.replace("==> ", "");
            else
                status = "完成";
        } else {
            status = "失败，展开细节查看输出";
            showDetails = true;
        }
    }

    component IconButton: Rectangle {
        id: button
        property string glyph: ""
        property bool active: false
        property color glyphColor: root.foregroundSoft
        signal clicked()

        width: 34
        height: 34
        radius: 10
        color: button.active ? root.surfaceHigh : (hover.hovered ? root.surfaceLow : "transparent")

        HoverHandler { id: hover }

        Text {
            anchors.centerIn: parent
            text: button.glyph
            color: button.active ? root.primary : button.glyphColor
            font.pixelSize: 15
        }

        MouseArea {
            anchors.fill: parent
            cursorShape: Qt.PointingHandCursor
            onClicked: button.clicked()
        }
    }

    component Pill: Rectangle {
        id: pill
        property string label: ""
        property bool busy: false
        signal clicked()

        implicitWidth: pillText.implicitWidth + 36
        implicitHeight: 38
        radius: 19
        color: pill.busy ? root.primary
             : (hover.hovered ? root.surfaceHigh : "transparent")
        border.width: pill.busy ? 0 : 1
        border.color: pill.busy ? root.primary
                    : (hover.hovered ? root.outline : root.outlineVariant)
        opacity: root.running && !pill.busy ? 0.45 : 1

        HoverHandler {
            id: hover
            enabled: !root.running
        }

        Text {
            id: pillText
            anchors.centerIn: parent
            text: pill.label
            color: pill.busy ? root.onPrimary : root.foregroundSoft
            font.pixelSize: 13
            font.weight: Font.Medium
        }

        MouseArea {
            anchors.fill: parent
            enabled: !root.running
            cursorShape: Qt.PointingHandCursor
            onClicked: pill.clicked()
        }
    }

    Process {
        id: runner
        running: false
        stdout: SplitParser {
            onRead: data => root.appendLog(data)
        }
        stderr: SplitParser {
            onRead: data => root.appendLog(data)
        }
        onExited: (code, status) => root.finish(code)
    }

    Timer {
        interval: 16
        repeat: true
        running: root.running
        onTriggered: root.phase = (root.phase + 0.018) % 1
    }

    FloatingWindow {
        id: win

        visible: true
        title: "使用 RED 打开"
        implicitWidth: 620
        implicitHeight: root.showDetails ? 410 : 248
        minimumSize: Qt.size(480, 240)
        color: "transparent"

        Rectangle {
            id: card
            anchors.fill: parent
            radius: 18
            color: root.surface
            border.width: 1
            border.color: root.outlineVariant
            clip: true

            Item {
                id: keyHandler
                anchors.fill: parent
                focus: true
                Keys.onEscapePressed: Qt.quit()
            }

            ColumnLayout {
                anchors.fill: parent
                anchors.margins: 24
                spacing: 16

                RowLayout {
                    Layout.fillWidth: true
                    spacing: 16

                    Text {
                        Layout.alignment: Qt.AlignTop
                        text: root.state.icon || "📦"
                        font.pixelSize: 58
                        font.family: "Noto Color Emoji"
                    }

                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 4

                        RowLayout {
                            Layout.fillWidth: true
                            spacing: 8

                            Rectangle {
                                visible: (root.state.badge || "") !== ""
                                implicitWidth: badgeText.implicitWidth + 16
                                implicitHeight: 20
                                radius: 6
                                color: root.tint(root.primary, 0.16)

                                Text {
                                    id: badgeText
                                    anchors.centerIn: parent
                                    text: root.state.badge || ""
                                    color: root.primary
                                    font.pixelSize: 10
                                    font.weight: Font.DemiBold
                                    font.family: "Noto Sans Mono"
                                }
                            }

                            Text {
                                Layout.fillWidth: true
                                text: "当前文件：" + (root.state.name || "")
                                color: root.foreground
                                font.pixelSize: 17
                                font.weight: Font.DemiBold
                                elide: Text.ElideMiddle
                            }
                        }

                        Text {
                            Layout.fillWidth: true
                            text: root.state.summary || ""
                            color: root.muted
                            font.pixelSize: 13
                            wrapMode: Text.WordWrap
                            maximumLineCount: 2
                            elide: Text.ElideRight
                        }

                        Text {
                            Layout.fillWidth: true
                            visible: root.state.size !== "" || root.state.arch !== ""
                            text: (root.state.size || "") + "  ·  " + (root.state.arch || "")
                            color: root.muted
                            font.pixelSize: 12
                            elide: Text.ElideRight
                        }

                        Item {
                            implicitWidth: detailLink.implicitWidth
                            implicitHeight: detailLink.implicitHeight + 4

                            Text {
                                id: detailLink
                                anchors.left: parent.left
                                anchors.verticalCenter: parent.verticalCenter
                                text: root.showDetails ? "细节 ˅" : "细节 ›"
                                color: linkHover.hovered ? root.foregroundSoft
                                     : (root.showDetails ? root.primary : root.muted)
                                font.pixelSize: 13

                                HoverHandler {
                                    id: linkHover
                                }
                            }

                            MouseArea {
                                anchors.fill: parent
                                cursorShape: Qt.PointingHandCursor
                                onClicked: root.showDetails = !root.showDetails
                            }
                        }
                    }

                    IconButton {
                        Layout.alignment: Qt.AlignTop
                        glyph: "✕"
                        glyphColor: root.foreground
                        onClicked: Qt.quit()
                    }
                }

                Row {
                    Layout.fillWidth: true
                    spacing: 10
                    visible: (root.state.actions || []).length > 0

                    Repeater {
                        model: root.state.actions || []

                        delegate: Pill {
                            required property var modelData
                            label: modelData.label
                            busy: root.activeAction === modelData.id
                            onClicked: root.start(modelData.id)
                        }
                    }
                }

                Rectangle {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 132
                    visible: root.showDetails
                    radius: 12
                    color: root.surfaceLow
                    border.width: 1
                    border.color: root.outlineVariant
                    clip: true

                    Flickable {
                        anchors.fill: parent
                        anchors.margins: 12
                        contentWidth: width
                        contentHeight: logText.implicitHeight
                        clip: true
                        boundsBehavior: Flickable.StopAtBounds

                        Text {
                            id: logText
                            width: parent.width
                            text: root.logLines.length > 0
                                  ? root.logLines.join("\n")
                                  : (root.state.detail || "")
                            color: root.foregroundSoft
                            font.pixelSize: 12
                            font.family: "Noto Sans Mono"
                            wrapMode: Text.WrapAnywhere
                        }
                    }
                }

                Item {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 10

                    Text {
                        Layout.fillWidth: true
                        visible: root.status !== ""
                        text: root.status
                        color: root.finished && !root.ok ? root.danger : root.foreground
                        font.pixelSize: 13
                        elide: Text.ElideRight
                    }

                    Rectangle {
                        id: track
                        Layout.fillWidth: true
                        height: 3
                        radius: 1.5
                        color: root.tint(root.foreground, 0.12)
                        clip: true

                        Rectangle {
                            id: bar
                            height: 3
                            radius: 1.5
                            color: root.finished && !root.ok ? root.danger : root.primary
                            width: root.running ? track.width * 0.4
                                 : (root.finished ? track.width : 0)
                            x: root.running
                               ? -width + root.phase * (track.width + width)
                               : 0
                        }
                    }
                }
            }
        }
    }
}
