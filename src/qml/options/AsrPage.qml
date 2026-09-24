import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Ripose.Memento

Page {
    id: root

    property int preferredWidth: 600
    property int groupSpacing: 10

    readonly property var backendOptions: {
        let options = [];
        if (Features.asrWhisper)
        {
            options.push({ label: qsTr("Whisper"), value: "whisper" });
        }
        return options;
    }
    readonly property var whisperModelOptions: [
        "tiny",
        "base",
        "small",
        "medium",
        "large-v3",
        "large-v3-turbo",
        "custom",
    ]
    readonly property var numericOptions: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
    readonly property var threadOptions: {
        let options = [];
        const maxThreads = MementoSettings.whisperMaxThreads;
        for (let i = 1; i <= maxThreads; ++i)
        {
            options.push(i);
        }
        return options;
    }

    function backendIndex(value) {
        for (let i = 0; i < root.backendOptions.length; ++i)
        {
            if (root.backendOptions[i].value === value)
            {
                return i;
            }
        }
        return 0;
    }

    function maybePromptModelDownload(model) {
        if (model === "custom" || AsrController.modelAvailable(model))
        {
            return;
        }

        asrDownloadPrompt.openForModel(model);
    }

    Loader {
        id: asrDownloadPrompt
        active: false

        function openForModel(model) {
            active = true;
            item.modelName = model;
            item.open();
        }

        sourceComponent: Component {
            Dialog {
                id: asrDownloadPromptItem

                property string modelName: ""

                parent: Overlay.overlay
                anchors.centerIn: parent
                modal: true
                standardButtons: Dialog.Yes | Dialog.No
                title: qsTr("Download ASR Model")

                onAccepted: asrDownloadDialog.start(modelName)

                Label {
                    width: 420
                    wrapMode: Text.WordWrap
                    text: qsTr(
                        "The selected ASR model is not installed in %1.\n\n" +
                        "Do you want to download %2 now?"
                    ).arg(AsrController.modelsDirectory())
                     .arg(asrDownloadPromptItem.modelName)
                }
            }
        }
    }

    Loader {
        id: asrDownloadDialog
        active: false
        sourceComponent: Component {
            AsrDownloadDialog { }
        }

        function start(model) {
            active = true;
            item.start(model);
        }
    }

    footer: ColumnLayout {
        spacing: 0

        Rectangle {
            Layout.fillWidth: true
            visible: Features.unix && !scrollView.atBottom
            color: MementoPalette.border
            height: 1
        }

        DialogButtonBox {
            id: buttonBoxFooter
            Layout.fillWidth: true
            standardButtons: DialogButtonBox.Apply |
                             DialogButtonBox.RestoreDefaults |
                             DialogButtonBox.Reset

            onApplied: {
                MementoSettings.writeAsrSettings();
                MementoSettings.writeWhisperSettings();
            }
            onClicked: function(button) {
                if (button === standardButton(DialogButtonBox.Reset))
                {
                    MementoSettings.loadAsrSettings();
                    MementoSettings.loadWhisperSettings();
                }
                else if (button === standardButton(DialogButtonBox.RestoreDefaults))
                {
                    MementoSettings.defaultAsrSettings();
                    MementoSettings.defaultWhisperSettings();
                }
            }
        }
    }

    ScrollView {
        id: scrollView
        anchors.fill: parent
        contentWidth: scrollView.contentWidth
        leftPadding: root.groupSpacing
        rightPadding: root.groupSpacing
        clip: true

        readonly property bool atBottom:
            (ScrollBar.vertical.position + ScrollBar.vertical.size) >= 0.99

        ColumnLayout {
            id: scrollViewLayout
            width: parent.width
            spacing: root.groupSpacing

            SettingsBox {
                Layout.preferredWidth: root.preferredWidth
                Layout.topMargin: root.groupSpacing
                Layout.bottomMargin: root.groupSpacing
                Layout.alignment: Qt.AlignHCenter
                title: qsTr("ASR")

                ColumnLayout {
                    anchors.fill: parent
                    spacing: root.groupSpacing

                    RowLayout {
                        Label {
                            Layout.fillWidth: true
                            Layout.alignment: Qt.AlignLeft
                            text: qsTr("Backend")
                        }
                        ComboBox {
                            Layout.alignment: Qt.AlignRight
                            Layout.preferredWidth: 250
                            model: root.backendOptions
                            textRole: "label"
                            valueRole: "value"
                            currentIndex: root.backendIndex(
                                MementoSettings.asrBackend
                            )
                            onActivated: MementoSettings.asrBackend =
                                currentValue
                        }
                    }

                    SettingsBoxSeparator { Layout.fillWidth: true }

                    RowLayout {
                        Label {
                            Layout.fillWidth: true
                            Layout.alignment: Qt.AlignLeft
                            text: qsTr("Enabled")
                        }
                        Switch {
                            Layout.alignment: Qt.AlignRight
                            checked: MementoSettings.asrEnabled
                            onClicked: MementoSettings.asrEnabled = checked
                        }
                    }

                    SettingsBoxSeparator { Layout.fillWidth: true }

                    Label {
                        visible: MementoSettings.asrBackend === "whisper"
                        Layout.fillWidth: true
                        text: qsTr("Whisper")
                        font.bold: true
                    }

                    RowLayout {
                        visible: MementoSettings.asrBackend === "whisper"
                        Label {
                            Layout.fillWidth: true
                            Layout.alignment: Qt.AlignLeft
                            text: qsTr("Use GPU")
                        }
                        Switch {
                            Layout.alignment: Qt.AlignRight
                            checked: MementoSettings.whisperUseGpu
                            onClicked: MementoSettings.whisperUseGpu = checked
                        }
                    }

                    SettingsBoxSeparator {
                        visible: MementoSettings.asrBackend === "whisper"
                        Layout.fillWidth: true
                    }

                    RowLayout {
                        visible: MementoSettings.asrBackend === "whisper"
                        Label {
                            Layout.fillWidth: true
                            Layout.alignment: Qt.AlignLeft
                            text: qsTr("Model")
                        }
                        ComboBox {
                            Layout.alignment: Qt.AlignRight
                            Layout.preferredWidth: 250
                            model: root.whisperModelOptions
                            currentIndex: Math.max(
                                0,
                                root.whisperModelOptions.indexOf(
                                    MementoSettings.whisperModel
                                )
                            )
                            onActivated: {
                                MementoSettings.whisperModel = currentValue;
                                root.maybePromptModelDownload(currentValue);
                            }
                        }
                    }

                    RowLayout {
                        visible: MementoSettings.asrBackend === "whisper" &&
                            MementoSettings.whisperModel === "custom"
                        Layout.fillWidth: true

                        Label {
                            Layout.fillWidth: true
                            Layout.alignment: Qt.AlignLeft
                            text: qsTr("Custom model")
                        }
                        TextField {
                            Layout.alignment: Qt.AlignRight
                            Layout.preferredWidth: 250
                            placeholderText: qsTr("Model path")
                            text: MementoSettings.whisperCustomModel
                            onEditingFinished: {
                                MementoSettings.whisperCustomModel = text;
                            }
                        }
                    }

                    SettingsBoxSeparator {
                        visible: MementoSettings.asrBackend === "whisper"
                        Layout.fillWidth: true
                    }

                    RowLayout {
                        visible: MementoSettings.asrBackend === "whisper"
                        Label {
                            Layout.fillWidth: true
                            Layout.alignment: Qt.AlignLeft
                            text: qsTr("VAD")
                        }
                        Switch {
                            Layout.alignment: Qt.AlignRight
                            checked: MementoSettings.whisperVadEnabled
                            onClicked: MementoSettings.whisperVadEnabled = checked
                        }
                    }

                    RowLayout {
                        visible: MementoSettings.asrBackend === "whisper" &&
                            MementoSettings.whisperVadEnabled
                        Layout.fillWidth: true

                        Label {
                            Layout.fillWidth: true
                            Layout.alignment: Qt.AlignLeft
                            text: qsTr("VAD model")
                        }
                        TextField {
                            Layout.alignment: Qt.AlignRight
                            Layout.preferredWidth: 250
                            placeholderText: qsTr("Model path")
                            text: MementoSettings.whisperVadModel
                            onEditingFinished: MementoSettings.whisperVadModel = text
                        }
                    }

                    SettingsBoxSeparator {
                        visible: MementoSettings.asrBackend === "whisper"
                        Layout.fillWidth: true
                    }

                    RowLayout {
                        visible: MementoSettings.asrBackend === "whisper"
                        Label {
                            Layout.fillWidth: true
                            Layout.alignment: Qt.AlignLeft
                            text: qsTr("Threads")
                        }
                        ComboBox {
                            Layout.alignment: Qt.AlignRight
                            Layout.preferredWidth: 250
                            model: root.threadOptions
                            currentIndex: Math.max(
                                0,
                                root.threadOptions.indexOf(
                                    MementoSettings.whisperThreads
                                )
                            )
                            onActivated: MementoSettings.whisperThreads =
                                currentValue
                        }
                    }

                    SettingsBoxSeparator {
                        visible: MementoSettings.asrBackend === "whisper"
                        Layout.fillWidth: true
                    }

                    RowLayout {
                        visible: MementoSettings.asrBackend === "whisper"
                        Label {
                            Layout.fillWidth: true
                            Layout.alignment: Qt.AlignLeft
                            text: qsTr("Best of")
                        }
                        ComboBox {
                            Layout.alignment: Qt.AlignRight
                            Layout.preferredWidth: 250
                            model: root.numericOptions
                            currentIndex: MementoSettings.whisperBestOf - 1
                            onActivated: MementoSettings.whisperBestOf =
                                currentValue
                        }
                    }

                    SettingsBoxSeparator {
                        visible: MementoSettings.asrBackend === "whisper"
                        Layout.fillWidth: true
                    }

                    RowLayout {
                        visible: MementoSettings.asrBackend === "whisper"
                        Label {
                            Layout.fillWidth: true
                            Layout.alignment: Qt.AlignLeft
                            text: qsTr("Beam search size")
                        }
                        ComboBox {
                            Layout.alignment: Qt.AlignRight
                            Layout.preferredWidth: 250
                            model: root.numericOptions
                            currentIndex: MementoSettings.whisperBeamSize - 1
                            onActivated: MementoSettings.whisperBeamSize =
                                currentValue
                        }
                    }

                    SettingsBoxSeparator {
                        visible: MementoSettings.asrBackend === "whisper"
                        Layout.fillWidth: true
                    }

                    RowLayout {
                        visible: MementoSettings.asrBackend === "whisper"
                        Label {
                            Layout.fillWidth: true
                            Layout.alignment: Qt.AlignLeft
                            text: qsTr("Flash attention")
                        }
                        Switch {
                            Layout.alignment: Qt.AlignRight
                            checked: MementoSettings.whisperFlashAttention
                            onClicked: {
                                MementoSettings.whisperFlashAttention = checked;
                            }
                        }
                    }

                }
            }
        }
    }
}
