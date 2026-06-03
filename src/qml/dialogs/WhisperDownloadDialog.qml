import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Ripose.Memento

Dialog {
    id: root

    property string model: ""
    property string error: ""
    property bool success: false

    signal finished(bool success, string error)

    parent: Overlay.overlay
    anchors.centerIn: parent
    modal: true
    width: 460
    closePolicy: WhisperController.downloadRunning ?
        Popup.NoAutoClose :
        Popup.CloseOnEscape
    title: qsTr("Download Whisper Model")

    component DialogFooterButton: Rectangle {
        id: footerButton

        signal clicked()

        property alias text: footerButtonText.text

        Layout.preferredWidth: Math.max(80, footerButtonText.implicitWidth + 28)
        Layout.preferredHeight: 32
        radius: 4
        color: footerButtonMouseArea.containsMouse ?
            MementoPalette.highlight :
            MementoPalette.mid
        border.color: MementoPalette.highlight
        border.width: 1

        Text {
            id: footerButtonText

            anchors.centerIn: parent
            color: MementoPalette.text
        }

        MouseArea {
            id: footerButtonMouseArea

            anchors.fill: parent
            hoverEnabled: true
            onClicked: footerButton.clicked()
        }
    }

    footer: RowLayout {
        visible: !WhisperController.downloadRunning
        width: parent.width
        spacing: 8

        Item {
            Layout.fillWidth: true
        }

        DialogFooterButton {
            text: qsTr("OK")
            onClicked: root.accept()
        }
    }

    function formatBytes(bytes) {
        if (bytes <= 0)
        {
            return qsTr("Unknown");
        }

        const units = ["B", "KB", "MB", "GB"];
        let value = bytes;
        let unit = 0;
        while (value >= 1024 && unit < units.length - 1)
        {
            value /= 1024;
            ++unit;
        }
        return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
    }

    function start(modelName) {
        root.model = modelName;
        root.error = "";
        root.success = false;
        root.open();

        WhisperController.downloadModel(modelName)
            .then(function(result) {
                root.error = result.error ?? "";
                root.success = !root.error;
                root.finished(root.success, root.error);
            });
    }

    ColumnLayout {
        id: contentLayout

        width: root.availableWidth
        spacing: 10

        Label {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            text: root.error ?
                root.error :
                (root.success ?
                    qsTr("Downloaded %1.").arg(root.model) :
                    qsTr("Downloading %1 to %2.")
                        .arg(root.model)
                        .arg(WhisperController.modelsDirectory()))
        }

        Rectangle {
            Layout.fillWidth: true
            implicitHeight: 6
            radius: height / 2
            color: MementoPalette.mid
            clip: true

            Rectangle {
                width: parent.width * WhisperController.downloadProgress
                height: parent.height
                radius: parent.radius
                color: MementoPalette.highlight
            }
        }

        GridLayout {
            Layout.fillWidth: true
            columns: 2
            columnSpacing: 16
            rowSpacing: 6

            Label {
                text: qsTr("Progress")
            }
            Label {
                Layout.fillWidth: true
                horizontalAlignment: Text.AlignRight
                text: `${Math.round(WhisperController.downloadProgress * 100)}%`
            }

            Label {
                text: qsTr("Downloaded")
            }
            Label {
                Layout.fillWidth: true
                horizontalAlignment: Text.AlignRight
                text: qsTr("%1 of %2")
                    .arg(root.formatBytes(WhisperController.downloadReceived))
                    .arg(root.formatBytes(WhisperController.downloadTotal))
            }

            Label {
                text: qsTr("Model size")
            }
            Label {
                Layout.fillWidth: true
                horizontalAlignment: Text.AlignRight
                text: root.formatBytes(WhisperController.downloadTotal)
            }

            Label {
                text: qsTr("Speed")
            }
            Label {
                Layout.fillWidth: true
                horizontalAlignment: Text.AlignRight
                text: qsTr("%1/s")
                    .arg(root.formatBytes(WhisperController.downloadSpeed))
            }
        }
    }
}
