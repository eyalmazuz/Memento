import QtQuick
import QtQuick.Controls

Slider {
    id: root

    property var chapters: []

    readonly property real xPosition: {
        if (root.pressed)
        {
            return Math.max(0,
                Math.min(
                    root.background.width,
                    pointHandler.point.position.x
                )
            );
        }
        else if (root.hovered)
        {
            return hoverHandler.point.position.x;
        }
        return -1;
    }

    readonly property real cursorValue: {
        if (root.xPosition !== -1)
        {
            const pos = root.xPosition / root.background.width;
            return root.valueAt(pos);
        }
        return -1;
    }

    implicitHeight: 30
    implicitWidth: 200
    leftPadding: 0
    topPadding: 0
    rightPadding: 0
    bottomPadding: 0
    leftInset: 0
    topInset: 0
    rightInset: 0
    bottomInset: 0

    HoverHandler {
        id: hoverHandler
        grabPermissions: PointerHandler.TakeOverForbidden
    }

    PointHandler {
        id: pointHandler
        grabPermissions: PointerHandler.TakeOverForbidden
    }

    handle: Item {
        implicitWidth: 1
        implicitHeight: root.implicitHeight
    }

    background: Rectangle {
        implicitWidth: root.implicitWidth
        implicitHeight: root.implicitHeight
        width: root.availableWidth
        color: MementoPalette.dark

        Rectangle {
            width: root.visualPosition * parent.width
            height: parent.height
            color: MementoPalette.accent
        }

        Repeater {
            model: root.chapters
            Rectangle {
                x: parent.width * (modelData / root.to)
                y: 0
                width: 1
                height: parent.height
                color: MementoPalette.window
            }
        }
    }

    onCursorValueChanged: {
        if (!root.enabled || root.cursorValue === -1)
        {
            if (player.thumbShow)
            {
                player.thumbShow = false;
                player.controller.scriptMessageTo(
                    "thumbfast",
                    [
                        "clear"
                    ]
                );
            }
        }
        else if (player.thumbAvailable)
        {
            player.controller.scriptMessageTo(
                "thumbfast",
                [
                    "thumb",
                    root.cursorValue.toString(),
                    "",
                    "",
                    player.controller.clientName()
                ]
            );
        }
    }

    Rectangle {
        id: thumbFrame

        readonly property int imageMargin: 2

        visible:
            player.thumbShow &&
            root.enabled &&
            (root.hovered || root.pressed)
        width: player.thumbWidth + imageMargin * 2
        height: player.thumbHeight + imageMargin * 2
        color: MementoPalette.window
        border.color: MementoPalette.border
        border.width: 1
        radius: 4
        anchors.bottom: parent.top
        anchors.bottomMargin: 10
        x: {
            const max_x = root.width - width;
            return Math.max(0, Math.min(max_x, root.xPosition - width / 2));
        }

        Image {
            id: thumbImage

            anchors.fill: parent
            anchors.margins: thumbFrame.imageMargin
            source: player.thumbFile
            asynchronous: true
            cache: false
        }
    }

    StrokeLabel {
        anchors.verticalCenter: parent.verticalCenter
        x: {
            const idealPosition = root.xPosition - width - 10;
            if (idealPosition < 10)
            {
                return root.xPosition + 15;
            }
            return idealPosition;
        }
        visible: root.enabled && (root.hovered || root.pressed)
        text: Utils.toTimeString(root.cursorValue)
        font.pixelSize: root.height
        color: MementoPalette.text
        stroke: MementoPalette.window
        strokeSize: 5
    }
}
