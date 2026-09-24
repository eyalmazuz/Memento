////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2026 Ripose
//
// This file is part of Memento.
//
// Memento is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, version 2 of the License.
//
// Memento is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Memento.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////

#include "player/mpvcontroller.h"

#include <cinttypes>
#include <cmath>
#include <cstdlib>
#include <cstring>
#include <limits>
#include <utility>

#include <QMetaObject>
#include <QScopeGuard>
#include <QThread>
#include <QTemporaryFile>
#include <QElapsedTimer>
#include <QtConcurrent>

#include "player/mpvplayer.h"
#include "player/mpvstate.h"
#include "util/directoryutils.h"

namespace
{

/**
 * @brief Runs a synchronous controller operation on its QObject thread.
 *
 * @param object The QObject whose thread owns the operation.
 * @param failure The value to return if the operation cannot be invoked.
 * @param function The operation to execute.
 * @return The operation result or the failure value.
 */
template<typename Result, typename Function>
Result invokeOnObjectThread(
    QObject *object, Result failure, Function &&function)
{
    if (QThread::currentThread() == object->thread())
    {
        return std::forward<Function>(function)();
    }

    Result result = failure;
    bool invoked = QMetaObject::invokeMethod(
        object,
        [&result, function = std::forward<Function>(function)]() mutable
        {
            result = function();
        },
        Qt::BlockingQueuedConnection
    );
    return invoked ? result : failure;
}

/**
 * @brief Checks cue timing and text against the in-memory subtitle limits.
 *
 * @param start The cue start time in seconds.
 * @param end The cue end time in seconds.
 * @param text The UTF-8 cue text.
 * @return true if the cue arguments are valid, false otherwise.
 */
bool subtitleCueIsValid(double start, double end, const QByteArray &text)
{
    constexpr qsizetype MAX_TEXT_BYTES = 1024 * 1024;
    constexpr double MAX_TIME_MS =
        static_cast<double>(std::numeric_limits<int>::max());

    return std::isfinite(start) &&
        std::isfinite(end) &&
        start >= 0.0 &&
        end > start &&
        end * 1000.0 <= MAX_TIME_MS &&
        text.size() <= MAX_TEXT_BYTES &&
        !text.contains('\0');
}

/**
 * @brief Checks an in-memory subtitle track or cue ID.
 *
 * @param id The track or cue ID to validate.
 * @return true if the ID is within the supported range, false otherwise.
 */
bool subtitleIdIsValid(qint64 id)
{
    return id > 0 && id <= std::numeric_limits<int>::max();
}

}

MpvController::MpvController(MpvPlayer *parent) : QObject(parent)
{
    setPlayer(parent);
    m_subtitleExtensions = {
        "ass",
        "idx",
        "lrc",
        "mks",
        "pgs",
        "rt",
        "scc",
        "smi",
        "srt",
        "ssa",
        "sub",
        "sup",
        "utf-8",
        "utf",
        "utf8",
        "vtt",
    };
}

/* Begin Public Functions */

MpvPlayer *MpvController::player() const noexcept
{
    return m_player;
}

void MpvController::setPlayer(MpvPlayer *value)
{
    if (m_player == value)
    {
        return;
    }
    m_player = value;
    setParent(m_player);
    emit playerChanged();
}

bool MpvController::loadFile(
    const QString &file,
    bool append,
    const QStringList &options)
{
    /* Since mpv client API 2.3 (mpv 0.38.0) "loadfile" places an extra argument
     * before the options, so we need to conditionally re-arrange the arguments
     * array */
    constexpr bool IS_API_23{MPV_CLIENT_API_VERSION >= MPV_MAKE_VERSION(2, 3)};

    if (file.isEmpty())
    {
        return false;
    }

    if (isSubtitleFile(file))
    {
        return loadSubtitle(file);
    }

    QByteArray path = file.toUtf8();
    QByteArray opts = options.join(',').toUtf8();
    char *argOpts = opts.isEmpty() ? nullptr : opts.data();

    const char *args[]{
        "loadfile",
        path,
        append ? "append-play" : "replace",
        IS_API_23 ? "0" : argOpts,
        IS_API_23 ? argOpts : nullptr,
        nullptr
    };

    int ret = ::mpv_command(handle(), args);
    if (ret < 0)
    {
        qWarning(
            "Could not open file '%s': %s",
            qUtf8Printable(file),
            ::mpv_error_string(ret)
        );
        return false;
    }
    return true;
}

bool MpvController::loadArgs(const QStringList &args)
{
    if (args.size() <= 1)
    {
        return false;
    }

    if (!argsValid(args))
    {
        qInfo() << tr("Invalid command line arguments.");
        return false;
    }
    LoadFileNode parent{};
    if (buildArgsTree(args, 1, parent) == -1)
    {
        qInfo() << tr("Maximum number of nested per-file arguments exceeded.");
        return false;
    }
    QStringList emptyOpts;
    loadFilesFromTree(parent, emptyOpts);
    return true;
}

void MpvController::loadFile(const QStringList &files, bool append)
{
    if (files.isEmpty())
    {
        return;
    }

    for (const QString &file : files)
    {
        if (isSubtitleFile(file))
        {
            loadSubtitle(file);
        }
        else
        {
            if (!append)
            {
                stop();
            }
            loadFile(file, append);
            append = true;
        }
    }
}

bool MpvController::loadSubtitle(const QString &file)
{
    if (file.isEmpty())
    {
        return false;
    }

    QByteArray path = file.toUtf8();
    const char *args[]{
        "sub-add",
        path,
        nullptr
    };
    int ret = ::mpv_command_async(handle(), 0, args);
    if (ret < 0)
    {
        qWarning(
            "Could not add subtitle file '%s': %s",
            qUtf8Printable(file),
            ::mpv_error_string(ret)
        );
        return false;
    }
    return true;
}

bool MpvController::supportsMemorySubtitles() const
{
    return invokeOnObjectThread<bool>(
        const_cast<MpvController *>(this),
        false,
        [this]()
        {
            if (m_player == nullptr || handle() == nullptr)
            {
                return false;
            }

            mpv_node result{};
            int ret = ::mpv_get_property(
                handle(), "command-list", MPV_FORMAT_NODE, &result
            );
            auto cleanup = qScopeGuard(
                [&result] { ::mpv_free_node_contents(&result); }
            );
            if (ret < 0 ||
                result.format != MPV_FORMAT_NODE_ARRAY ||
                result.u.list == nullptr)
            {
                return false;
            }

            static constexpr const char *REQUIRED_COMMANDS[]{
                "sub-create",
                "add-subtitle",
                "edit-subtitle",
                "remove-subtitle",
                "clear-subtitles",
                "sub-remove",
            };
            for (const char *required : REQUIRED_COMMANDS)
            {
                bool found = false;
                mpv_node_list *commands = result.u.list;
                for (int i = 0; i < commands->num; ++i)
                {
                    if (mapString(commands->values[i], "name") ==
                        QString::fromLatin1(required))
                    {
                        found = true;
                        break;
                    }
                }
                if (!found)
                {
                    return false;
                }
            }
            return true;
        }
    );
}

qint64 MpvController::createSubtitleTrack(
    const QString &title, const QString &language)
{
    return invokeOnObjectThread<qint64>(
        this,
        -1,
        [this, title, language]() -> qint64
        {
            if (!supportsMemorySubtitles() ||
                m_player == nullptr ||
                !m_player->isFileLoaded())
            {
                return -1;
            }

            QByteArray titleUtf8 = title.toUtf8();
            QByteArray languageUtf8 = language.toUtf8();
            if (titleUtf8.contains('\0') || languageUtf8.contains('\0'))
            {
                return -1;
            }

            const char *args[]{
                "sub-create",
                titleUtf8.constData(),
                languageUtf8.constData(),
                nullptr,
            };
            mpv_node result{};
            int ret = ::mpv_command_ret(handle(), args, &result);
            auto cleanup = qScopeGuard(
                [&result] { ::mpv_free_node_contents(&result); }
            );
            if (ret < 0)
            {
                qWarning(
                    "Could not create in-memory subtitle track: %s",
                    ::mpv_error_string(ret)
                );
                return -1;
            }
            if (result.format != MPV_FORMAT_INT64 ||
                !subtitleIdIsValid(result.u.int64))
            {
                qWarning(
                    "In-memory subtitle track command returned no track ID"
                );
                return -1;
            }
            return result.u.int64;
        }
    );
}

qint64 MpvController::addSubtitle(
    qint64 trackId, double start, double end, const QString &text)
{
    return invokeOnObjectThread<qint64>(
        this,
        -1,
        [this, trackId, start, end, text]() -> qint64
        {
            QByteArray textUtf8 = text.toUtf8();
            if (!supportsMemorySubtitles() ||
                !subtitleIdIsValid(trackId) ||
                !subtitleCueIsValid(start, end, textUtf8))
            {
                return -1;
            }

            QByteArray trackIdUtf8 =
                QByteArray::number(static_cast<qlonglong>(trackId));
            QByteArray startUtf8 = QByteArray::number(start, 'g', 17);
            QByteArray endUtf8 = QByteArray::number(end, 'g', 17);
            const char *args[]{
                "add-subtitle",
                trackIdUtf8.constData(),
                startUtf8.constData(),
                endUtf8.constData(),
                textUtf8.constData(),
                nullptr,
            };
            mpv_node result{};
            int ret = ::mpv_command_ret(handle(), args, &result);
            auto cleanup = qScopeGuard(
                [&result] { ::mpv_free_node_contents(&result); }
            );
            if (ret < 0)
            {
                qWarning(
                    "Could not add in-memory subtitle cue: %s",
                    ::mpv_error_string(ret)
                );
                return -1;
            }
            if (result.format != MPV_FORMAT_INT64 ||
                !subtitleIdIsValid(result.u.int64))
            {
                qWarning("Add-subtitle command returned no cue ID");
                return -1;
            }
            return result.u.int64;
        }
    );
}

bool MpvController::editSubtitle(
    qint64 trackId,
    qint64 cueId,
    double start,
    double end,
    const QString &text)
{
    return invokeOnObjectThread<bool>(
        this,
        false,
        [this, trackId, cueId, start, end, text]()
        {
            QByteArray textUtf8 = text.toUtf8();
            if (!supportsMemorySubtitles() ||
                !subtitleIdIsValid(trackId) ||
                !subtitleIdIsValid(cueId) ||
                !subtitleCueIsValid(start, end, textUtf8))
            {
                return false;
            }

            QByteArray trackIdUtf8 =
                QByteArray::number(static_cast<qlonglong>(trackId));
            QByteArray cueIdUtf8 =
                QByteArray::number(static_cast<qlonglong>(cueId));
            QByteArray startUtf8 = QByteArray::number(start, 'g', 17);
            QByteArray endUtf8 = QByteArray::number(end, 'g', 17);
            const char *args[]{
                "edit-subtitle",
                trackIdUtf8.constData(),
                cueIdUtf8.constData(),
                startUtf8.constData(),
                endUtf8.constData(),
                textUtf8.constData(),
                nullptr,
            };
            int ret = ::mpv_command(handle(), args);
            if (ret < 0)
            {
                qWarning(
                    "Could not edit in-memory subtitle cue: %s",
                    ::mpv_error_string(ret)
                );
                return false;
            }
            return true;
        }
    );
}

bool MpvController::removeSubtitle(qint64 trackId, qint64 cueId)
{
    return invokeOnObjectThread<bool>(
        this,
        false,
        [this, trackId, cueId]()
        {
            if (!supportsMemorySubtitles() ||
                !subtitleIdIsValid(trackId) ||
                !subtitleIdIsValid(cueId))
            {
                return false;
            }

            QByteArray trackIdUtf8 =
                QByteArray::number(static_cast<qlonglong>(trackId));
            QByteArray cueIdUtf8 = QByteArray::number(cueId);
            const char *args[]{
                "remove-subtitle",
                trackIdUtf8.constData(),
                cueIdUtf8.constData(),
                nullptr,
            };
            int ret = ::mpv_command(handle(), args);
            if (ret < 0)
            {
                qWarning(
                    "Could not remove in-memory subtitle cue: %s",
                    ::mpv_error_string(ret)
                );
                return false;
            }
            return true;
        }
    );
}

bool MpvController::clearSubtitles(qint64 trackId)
{
    return invokeOnObjectThread<bool>(
        this,
        false,
        [this, trackId]()
        {
            if (!supportsMemorySubtitles() || !subtitleIdIsValid(trackId))
            {
                return false;
            }

            QByteArray trackIdUtf8 =
                QByteArray::number(static_cast<qlonglong>(trackId));
            const char *args[]{
                "clear-subtitles",
                trackIdUtf8.constData(),
                nullptr,
            };
            int ret = ::mpv_command(handle(), args);
            if (ret < 0)
            {
                qWarning(
                    "Could not clear in-memory subtitle track: %s",
                    ::mpv_error_string(ret)
                );
                return false;
            }
            return true;
        }
    );
}

bool MpvController::removeSubtitleTrack(qint64 trackId)
{
    return invokeOnObjectThread<bool>(
        this,
        false,
        [this, trackId]()
        {
            if (!supportsMemorySubtitles() || !subtitleIdIsValid(trackId))
            {
                return false;
            }

            QByteArray trackIdUtf8 =
                QByteArray::number(static_cast<qlonglong>(trackId));
            const char *args[]{
                "sub-remove",
                trackIdUtf8.constData(),
                nullptr,
            };
            int ret = ::mpv_command(handle(), args);
            if (ret < 0)
            {
                qWarning(
                    "Could not remove in-memory subtitle track: %s",
                    ::mpv_error_string(ret)
                );
                return false;
            }
            return true;
        }
    );
}

void MpvController::seek(double time)
{
    QByteArray timestr = QString::number(time).toUtf8();
    const char *args[]{
        "seek",
        timestr,
        "absolute",
        NULL
    };
    if (::mpv_command_async(handle(), 0, args) < 0)
    {
        qWarning("Seeking failed");
        return;
    }
    emit seekRequested(time);
}

void MpvController::play()
{
    int flag = 0;
    if (::mpv_set_property(handle(), "pause", MPV_FORMAT_FLAG, &flag) < 0)
    {
        qWarning("Could not set mpv pause property to %d", flag);
    }
}

void MpvController::pause()
{
    int flag = 1;
    if (::mpv_set_property(handle(), "pause", MPV_FORMAT_FLAG, &flag) < 0)
    {
        qWarning("Could not set mpv pause property to %d", flag);
    }
}

void MpvController::stop()
{
    const char *args[]{
        "stop",
        nullptr
    };
    if (::mpv_command(handle(), args) < 0)
    {
        qWarning("Could not stop mpv");
    }
}

void MpvController::subtitleSeek(int count, bool primary)
{
    QByteArray countStr = QString::number(count).toUtf8();
    const char *args[]{
        "sub-seek",
        countStr,
        primary ? "primary" : "secondary",
        nullptr
    };
    if (::mpv_command_async(handle(), 0, args) < 0)
    {
        qWarning(
            "Could not seek %d subtitles in the %s track",
            count,
            primary ? "primary" : "secondary"
        );
    }
}

void MpvController::playlistNext()
{
    const char *args[]{
        "playlist-next",
        nullptr
    };
    if (::mpv_command_async(handle(), 0, args) < 0)
    {
        qWarning("Could not skip to the next file in the playlist");
    }
}

void MpvController::playlistPrev()
{
    const char *args[]{
        "playlist-prev",
        nullptr
    };
    if (::mpv_command_async(handle(), 0, args) < 0)
    {
        qWarning("Could not skip to the next file in the playlist");
    }
}

void MpvController::setAid(int64_t id)
{
    if (::mpv_set_property_async(handle(), 0, "aid", MPV_FORMAT_INT64, &id) < 0)
    {
        qWarning("Could not set audio track to %" PRId64, id);
    }
}

void MpvController::setSid(int64_t id)
{
    if (::mpv_set_property_async(handle(), 0, "sid", MPV_FORMAT_INT64, &id) < 0)
    {
        qWarning("Could not set subtitle track to %" PRId64, id);
    }
}

void MpvController::setSecondarySid(int64_t id)
{
    if (::mpv_set_property_async(handle(), 0, "secondary-sid", MPV_FORMAT_INT64, &id) < 0)
    {
        qWarning("Could not set secondary subtitle track to %" PRId64, id);
    }
}

void MpvController::setVid(int64_t id)
{
    if (::mpv_set_property_async(handle(), 0, "vid", MPV_FORMAT_INT64, &id) < 0)
    {
        qWarning("Could not set video track to %" PRId64, id);
    }
}

void MpvController::setSubtitleVisibility(bool visible)
{
    int flag = visible ? 1 : 0;
    if (::mpv_set_property_async(handle(), 0, "sub-visibility", MPV_FORMAT_FLAG, &flag) < 0)
    {
        qWarning("Could not set subtitle visibility");
    }
}

void MpvController::setSecondarySubtitleVisibility(bool visible)
{
    int flag = visible ? 1 : 0;
    if (::mpv_set_property_async(handle(), 0, "secondary-sub-visibility", MPV_FORMAT_FLAG, &flag) < 0)
    {
        qWarning("Could not set secondary subtitle visibility");
    }
}

void MpvController::setSubtitleDelay(double delay)
{
    if (::mpv_set_property_async(handle(), 0, "sub-delay", MPV_FORMAT_DOUBLE, &delay) < 0)
    {
        qWarning("Could not set subtitle delay to %lf", delay);
    }
}

void MpvController::setSecondarySubtitleDelay(double delay)
{
    if (::mpv_set_property_async(handle(), 0, "secondary-sub-delay", MPV_FORMAT_DOUBLE, &delay) < 0)
    {
        qWarning("Could not set secondary subtitle delay to %lf", delay);
    }
}

void MpvController::setFullscreen(bool value)
{
    int flag = value ? 1 : 0;
    if (mpv_set_property_async(handle(), 0, "fullscreen", MPV_FORMAT_FLAG, &flag) < 0)
    {
        qWarning("Could not set fullscreen");
    }
}

void MpvController::setVolume(int64_t value)
{
    if (::mpv_set_property_async(handle(), 0, "volume", MPV_FORMAT_INT64, &value) < 0)
    {
        qWarning("Could not set volume to %" PRId64, value);
    }
}

void MpvController::showText(const QString &text)
{
    QByteArray utf8Text = text.toUtf8();
    const char *command[] = {
        "show-text",
        utf8Text.data(),
        NULL
    };
    if (::mpv_command_async(handle(), 0, command))
    {
        qWarning("Could not show text '%s'", qUtf8Printable(text));
    }
}

void MpvController::sendKeyPress(int key, int modifiers)
{
    QString keypress = toModifierString(modifiers);
#if !defined(Q_OS_MACOS)
    if (modifiers & Qt::KeypadModifier)
    {
        switch (key)
        {
        case Qt::Key_0:
        case Qt::Key_1:
        case Qt::Key_2:
        case Qt::Key_3:
        case Qt::Key_4:
        case Qt::Key_5:
        case Qt::Key_6:
        case Qt::Key_7:
        case Qt::Key_8:
        case Qt::Key_9:
            keypress += "KP";
            break;
        case Qt::Key_Delete:
        case Qt::Key_Insert:
        case Qt::Key_Home:
        case Qt::Key_End:
        case Qt::Key_Clear:
        case Qt::Key_PageUp:
        case Qt::Key_PageDown:
        case Qt::Key_Right:
        case Qt::Key_Left:
        case Qt::Key_Down:
        case Qt::Key_Up:
        case Qt::Key_Enter:
            keypress += "KP_";
            break;
        }
    }
#endif // !defined(Q_OS_MACOS)

    if (key >= Qt::Key::Key_F1 && key <= Qt::Key::Key_F30)
    {
        keypress += "F" + QString::number(key - Qt::Key::Key_F1 + 1);
    }
    else
    {
        switch (key)
        {
        case Qt::Key::Key_Shift:
        case Qt::Key::Key_Control:
        case Qt::Key::Key_Alt:
        case Qt::Key::Key_Meta:
            return;
        case Qt::Key::Key_Left:
            keypress += "LEFT";
            break;
        case Qt::Key::Key_Right:
            keypress += "RIGHT";
            break;
        case Qt::Key::Key_Up:
            keypress += "UP";
            break;
        case Qt::Key::Key_Down:
            keypress += "DOWN";
            break;
        case Qt::Key::Key_Enter:
        case Qt::Key::Key_Return:
            keypress += "ENTER";
            break;
        case Qt::Key::Key_Escape:
            keypress += "ESC";
            break;
        case Qt::Key::Key_Backspace:
            keypress += "BS";
            break;
        case Qt::Key::Key_Pause:
            keypress += "PAUSE";
            break;
        case Qt::Key::Key_Delete:
            keypress += "DEL";
            break;
        case Qt::Key::Key_Insert:
            keypress += "INS";
            break;
        case Qt::Key::Key_Home:
            keypress += "HOME";
            break;
        case Qt::Key::Key_End:
            keypress += "END";
            break;
        case Qt::Key::Key_Clear:
            keypress += "DEC";
            break;
        case Qt::Key::Key_PageUp:
            keypress += "PGUP";
            break;
        case Qt::Key::Key_PageDown:
            keypress += "PGDWN";
            break;
        default:
        {
            QString text{QKeySequence(key).toString()};
            keypress += modifiers & Qt::ShiftModifier ?
                text : text.toLower();
        }
        }
    }

    QByteArray keypressUtf8 = keypress.toUtf8();
    const char *args[]{
        "keypress",
        keypressUtf8,
        nullptr
    };
    if (::mpv_command_async(handle(), 0, args) < 0)
    {
        qWarning("Could not send keypress for '%s'", qUtf8Printable(keypress));
    }
}

void MpvController::sendMouse(double x, double y)
{
    QByteArray xArg = QString::number(static_cast<int>(x)).toUtf8();
    QByteArray yArg = QString::number(static_cast<int>(y)).toUtf8();
    const char *args[]{
        "mouse",
        xArg,
        yArg,
        nullptr
    };
    if (::mpv_command_async(handle(), 0, args) < 0)
    {
        qWarning("Could not send mouse movement");
    }
}

void MpvController::sendMouseButton(
    double x,
    double y,
    Qt::MouseButton button,
    int modifiers,
    bool pressed)
{
    QString buttonName = toMouseButtonString(button);
    if (buttonName.isEmpty())
    {
        return;
    }

    sendMouse(x, y);

    if (pressed)
    {
        QByteArray key = (toModifierString(modifiers) + buttonName).toUtf8();

        /* Recover from a duplicate press without a corresponding release. */
        auto previous = m_pressedMouseButtons.find(button);
        if (previous != m_pressedMouseButtons.end())
        {
            sendMouseKey(previous.value(), false);
        }

        if (sendMouseKey(key, true))
        {
            m_pressedMouseButtons.insert(button, key);
        }
    }
    else
    {
        QByteArray key = (toModifierString(modifiers) + buttonName).toUtf8();
        auto previous = m_pressedMouseButtons.find(button);
        if (previous != m_pressedMouseButtons.end())
        {
            key = previous.value();
            m_pressedMouseButtons.erase(previous);
        }
        sendMouseKey(key, false);
    }
}

void MpvController::releaseMouseButtons()
{
    for (const QByteArray &key : std::as_const(m_pressedMouseButtons))
    {
        sendMouseKey(key, false);
    }
    m_pressedMouseButtons.clear();
}

void MpvController::sendWheel(double x, double y, QPoint angleDelta)
{
    QByteArray buttonArg;
    if (angleDelta.y() > 0)
    {
        buttonArg = "3";
    }
    else if (angleDelta.y() < 0)
    {
        buttonArg = "4";
    }
    else if (angleDelta.x() < 0)
    {
        buttonArg = "5";
    }
    else if (angleDelta.x() > 0)
    {
        buttonArg = "6";
    }
    else
    {
        return;
    }
    QByteArray xArg = QString::number(static_cast<int>(x)).toUtf8();
    QByteArray yArg = QString::number(static_cast<int>(y)).toUtf8();
    const char *mouseArgs[]{
        "mouse",
        xArg,
        yArg,
        buttonArg,
        nullptr
    };
    if (::mpv_command_async(handle(), 0, mouseArgs) < 0)
    {
        qWarning("Could not send mouse button");
    }
}


QString MpvController::tempScreenshot(bool subtitles, const QString &ext)
{
    /* Create a valid temporary file name */
    QTemporaryFile file;
    if (!file.open())
    {
        return {};
    }
    QByteArray filename = (file.fileName() + ext).toUtf8();
    file.close();

    const char *args[] = {
        "screenshot-to-file",
        filename,
        subtitles ? NULL : "video",
        NULL
    };
    if (mpv_command(handle(), args) < 0)
    {
        qWarning("Could not take temporary screenshot");
        return {};
    }

    return filename;
}

QImage MpvController::screenshotRaw(bool subtitles)
{
    const char *args[] = {
        "screenshot-raw",
        subtitles ? "subtitles" : "video",
        "rgba",
        NULL,
    };

    mpv_node result{};
    int ret = ::mpv_command_ret(handle(), args, &result);
    if (ret < 0)
    {
        qWarning("Could not take raw screenshot: %s", ::mpv_error_string(ret));
        return {};
    }

    QImage image;
    auto cleanup = qScopeGuard(
        [&result] { ::mpv_free_node_contents(&result); }
    );

    std::optional<int64_t> width = mapInt(result, "w");
    std::optional<int64_t> height = mapInt(result, "h");
    std::optional<int64_t> stride = mapInt(result, "stride");
    QString format = mapString(result, "format");
    const mpv_byte_array *data = mapByteArray(result, "data");
    if (!width ||
        !height ||
        !stride ||
        format != "rgba" ||
        data == nullptr ||
        data->data == nullptr)
    {
        qWarning("Raw screenshot result had an unexpected format");
        return {};
    }
    if (*width <= 0 ||
        *height <= 0 ||
        *width > std::numeric_limits<int>::max() ||
        *height > std::numeric_limits<int>::max())
    {
        qWarning("Raw screenshot result had invalid dimensions");
        return {};
    }

    constexpr int BYTES_PER_PIXEL = 4;
    const int64_t minimumLineSize = *width * BYTES_PER_PIXEL;
    if (std::llabs(*stride) < minimumLineSize)
    {
        qWarning("Raw screenshot result had an invalid stride");
        return {};
    }

    image = QImage(
        static_cast<int>(*width),
        static_cast<int>(*height),
        QImage::Format_RGBA8888
    );
    if (image.isNull())
    {
        qWarning("Could not allocate raw screenshot image");
        return {};
    }

    const char *source = static_cast<const char *>(data->data);
    for (int y = 0; y < image.height(); ++y)
    {
        std::memcpy(
            image.scanLine(y),
            source + y * *stride,
            static_cast<size_t>(minimumLineSize)
        );
    }

    return image;
}

QString MpvController::tempAudioClip(const MpvAudioClipArgs &args)
{
    return tempAudioClip(captureEncodeContext(), args, {});
}

QFuture<QString> MpvController::tempAudioClipAsync(
    const MpvAudioClipArgs &args,
    std::shared_ptr<std::atomic_bool> abort)
{
    const EncodeContext context = invokeOnObjectThread<EncodeContext>(
        this,
        {},
        [this] { return captureEncodeContext(); }
    );
    return QtConcurrent::run(
        [context, args, abort = std::move(abort)]
        {
            QElapsedTimer timer;
            timer.start();
            const QString path = MpvController::tempAudioClip(
                context, args, abort
            );
            qInfo(
                "TEMP_AUDIO_PERF elapsed_ms=%lld duration_s=%.3f canceled=%d",
                static_cast<long long>(timer.elapsed()),
                args.end - args.start,
                abort && abort->load()
            );
            return path;
        }
    );
}

QString MpvController::tempAudioClip(
    const EncodeContext &context,
    const MpvAudioClipArgs &args,
    std::shared_ptr<std::atomic_bool> abort)
{
    if (context.path.isEmpty() || context.audioId < 0 ||
        (abort && abort->load()))
    {
        if (!abort || !abort->load())
        {
            qWarning(
                "Could not extract audio: media path is %s and audio ID is %lld",
                context.path.isEmpty() ? "empty" : "available",
                static_cast<long long>(context.audioId)
            );
        }
        return {};
    }

    QString extension = args.extension;
    QByteArray argString = QString("start=%1,end=%2,aid=%3")
        .arg(args.start, 0, 'f', 3)
        .arg(args.end, 0, 'f', 3)
        .arg(context.audioId)
        .toUtf8();

    QList<QPair<QByteArray, QByteArray>> options = {
        {"vid", "no"},
        {"sid", "no"},
        {"secondary-sid", "no"},
    };

    switch (args.preset)
    {
        case MpvAudioClipArgs::WhisperPcm16Wav:
            extension = ".wav";
            options.emplaceBack("oac", "pcm_s16le");
            options.emplaceBack("audio-samplerate", "16000");
            options.emplaceBack("audio-channels", "mono");
            break;

        case MpvAudioClipArgs::Default:
            break;
    }

    if (args.normalize)
    {
        QByteArray audioFilter;
        audioFilter = "loudnorm=I=";
        audioFilter += QByteArray::number(args.db, 'f', 1);
        options.emplaceBack("af", std::move(audioFilter));
    }

    return encodeFile(
        context.path,
        context.scriptOptions,
        argString,
        options,
        extension,
        std::move(abort)
    );
}

QString MpvController::tempVideoClip(const MpvVideoClipArgs &args)
{
    constexpr const char *FILE_EXTENSION = ".mp4";

    const EncodeContext context = captureEncodeContext();
    if (context.path.isEmpty())
    {
        return {};
    }
    QByteArray argString = QString("ovc=libx264,oac=aac,start=%1,end=%2")
        .arg(args.start, 0, 'f', 3)
        .arg(args.end, 0, 'f', 3)
        .toUtf8();
    if (args.audio)
    {
        if (context.audioId < 0)
        {
            return {};
        }
        argString += QString(",aid=%1").arg(context.audioId).toUtf8();
    }
    if (args.subtitles)
    {
        int64_t sid = player()->state()->sid();
        if (sid == -1)
        {
            return {};
        }
        argString += QString(",sid=%1").arg(sid).toUtf8();
    }

    QList<QPair<QByteArray, QByteArray>> options = {
        {"vid", "yes"},
        {"aid", args.audio ? "yes" : "no"},
        {"sid", args.subtitles ? "yes" : "no"},
        {"secondary-sid", "no"},
    };
    if (args.normalize)
    {
        options.emplaceBack(
            "af",
            QString("loudnorm=I=%1").arg(args.db, 'f', 1).toUtf8()
        );
    }

    return encodeFile(
        context.path,
        context.scriptOptions,
        argString,
        options,
        FILE_EXTENSION,
        {}
    );
}

/* End Public Functions */
/* Begin Private Functions */

mpv_handle *MpvController::handle() const noexcept
{
    return player()->handle();
}

MpvController::EncodeContext MpvController::captureEncodeContext() const
{
    EncodeContext context;
    MpvPlayer *mpvPlayer = m_player;
    MpvState *state = mpvPlayer == nullptr ? nullptr : mpvPlayer->state();
    if (state == nullptr || mpvPlayer->handle() == nullptr)
    {
        return context;
    }

    context.path = state->path();
    context.audioId = state->aid();

    char *scriptOptions = ::mpv_get_property_string(
        mpvPlayer->handle(), "script-opts"
    );
    if (scriptOptions != nullptr)
    {
        const QByteArray currentOptions(scriptOptions);
        if (currentOptions.contains("ytdl_hook-ytdl_path="))
        {
            context.scriptOptions = currentOptions;
        }
    }
    ::mpv_free(scriptOptions);

#if MEMENTO_BUNDLE
    if (context.scriptOptions.isEmpty())
    {
        QByteArray ytdlPath = "ytdl_hook-ytdl_path=";
        char *configDir = ::mpv_get_property_string(
            mpvPlayer->handle(), "config-dir"
        );
        if (configDir != nullptr)
        {
            ytdlPath += configDir;
            ytdlPath += '/';
        }
        else
        {
            ytdlPath += DirectoryUtils::getConfigDir().toUtf8();
        }
        ::mpv_free(configDir);
        ytdlPath += "youtube-dl";
        context.scriptOptions = std::move(ytdlPath);
    }
#endif // MEMENTO_BUNDLE

    return context;
}

QString MpvController::quoteArg(QString arg)
{
    qsizetype eqidx = arg.indexOf('=');
    if (eqidx == -1)
    {
        return arg;
    }
    return arg.insert(eqidx + 1, "\"").append("\"");
}

bool MpvController::argsValid(const QStringList &args)
{
    qsizetype count = 0;
    for (qsizetype i = 1; i < args.size(); ++i)
    {
        if (args[i] == "--{")
        {
            count++;
        }
        else if (args[i] == "--}")
        {
            count--;
            if (count < 0)
            {
                return false;
            }
        }
    }

    return count == 0;
}

qsizetype MpvController::buildArgsTree(
    const QStringList &args, qsizetype i, LoadFileNode &parent, qsizetype depth)
{
    if (depth < 1)
    {
        return -1;
    }

    while (i < args.size())
    {
        if (args[i] == "--}")
        {
            return i;
        }
        else if (args[i] == "--{")
        {
            parent.files.emplaceBack("--"); // Used to indicate a level deeper
            parent.children.emplaceBack();
            LoadFileNode &child = parent.children.last();
            i = buildArgsTree(args, i + 1, child, depth - 1);
            if (i < 0)
            {
                return -1;
            }
        }
        else if (args[i].startsWith("--"))
        {
            parent.options.emplaceBack(quoteArg(args[i].mid(2)));
        }
        else
        {
            parent.files.emplaceBack(args[i]);
        }
        ++i;
    }

    return i;
}

void MpvController::loadFilesFromTree(
    const LoadFileNode &parent, QStringList &options)
{
    qsizetype lastSize = options.size();
    options.append(parent.options);

    qsizetype currentChild = 0;
    for (const QString &file : parent.files)
    {
        if (file == "--")
        {
            loadFilesFromTree(parent.children[currentChild], options);
            currentChild++;
        }
        else
        {
            loadFile(file, true, options);
        }
    }

    while (options.size() > lastSize)
    {
        options.removeLast();
    }
}

bool MpvController::isSubtitleFile(const QString &file) const
{
    QString ext =
        file.right(file.size() - file.lastIndexOf('.') - 1).toLower();
    return m_subtitleExtensions.contains(ext);
}

QString MpvController::toModifierString(int modifiers)
{
    QString keypress;
    if (modifiers & Qt::ShiftModifier)
    {
        keypress += "Shift+";
    }
    if (modifiers & Qt::ControlModifier)
    {
        keypress += "Ctrl+";
    }
    if (modifiers & Qt::AltModifier)
    {
        keypress += "Alt+";
    }
    if (modifiers & Qt::MetaModifier)
    {
        keypress += "Meta+";
    }
    return keypress;
}

QString MpvController::toMouseButtonString(Qt::MouseButton button)
{
    switch (button)
    {
    case Qt::MouseButton::LeftButton:
        return "MOUSE_BTN0";
    case Qt::MouseButton::MiddleButton:
        return "MOUSE_BTN1";
    case Qt::MouseButton::RightButton:
        return "MOUSE_BTN2";
    case Qt::MouseButton::BackButton:
        return "MOUSE_BTN7";
    case Qt::MouseButton::ForwardButton:
        return "MOUSE_BTN8";
    default:
        return {};
    }
}

bool MpvController::sendMouseKey(const QByteArray &key, bool pressed)
{
    const char *args[]{
        pressed ? "keydown" : "keyup",
        key,
        nullptr
    };
    if (::mpv_command_async(handle(), 0, args) < 0)
    {
        qWarning(
            "Could not send mouse button %s for '%s'",
            pressed ? "press" : "release",
            key.constData()
        );
        return false;
    }
    return true;
}

QString MpvController::encodeFile(
    const QString &inputPath,
    const QByteArray &scriptOptions,
    const QByteArray &argString,
    const QList<QPair<QByteArray, QByteArray>> &options,
    const QString &fileExtension,
    std::shared_ptr<std::atomic_bool> abort)
{
    if (abort && abort->load())
    {
        return {};
    }

    /* Create a valid temporary file name */
    QTemporaryFile file;
    if (!file.open())
    {
        qWarning("Could not create a temporary audio output file");
        return {};
    }
    QByteArray filename = (file.fileName() + fileExtension).toUtf8();
    const QString outputPath = QString::fromUtf8(filename);
    file.close();

    QByteArray input = inputPath.toUtf8();
    const char *argOpts = argString.isEmpty() ? NULL : argString.data();

    bool isApi23 = mpv_client_api_version() >= MPV_MAKE_VERSION(2, 3);
    const char *args[] = {
        "loadfile",
        input,
        "replace",
        isApi23 ? "0"     : argOpts,
        isApi23 ? argOpts : NULL,
        NULL
    };

    mpv_event *event = nullptr;
    mpv_handle *enc_h = nullptr;
    bool initialized = false;
    bool optionsSet = true;
    int initializeStatus = 0;
    int commandStatus = 0;
    const auto setOption = [&enc_h]
        (const QByteArray &name, const QByteArray &value)
        {
            const int status = ::mpv_set_option_string(
                enc_h, name.constData(), value.constData()
            );
            if (status < 0)
            {
                qWarning(
                    "Could not set mpv encoder option '%s': %s",
                    name.constData(),
                    ::mpv_error_string(status)
                );
            }
            return status >= 0;
        };

    const bool isNetwork = inputPath.contains("://");
    enc_h = mpv_create();
    if (enc_h == NULL)
    {
        qWarning("Error creating encoder handle");
        filename = "";
        goto cleanup;
    }
    ::mpv_request_log_messages(enc_h, "warn");

    for (const auto &[k, v] : options)
    {
        if (!setOption(k, v))
        {
            optionsSet = false;
            break;
        }
    }
    if (!optionsSet ||
        !setOption("cover-art-auto", "no") ||
        !setOption("keep-open", "no") ||
        !setOption("ytdl", isNetwork ? "yes" : "no") ||
        !setOption("config", "no") ||
        !setOption("o", filename))
    {
        filename.clear();
        goto cleanup;
    }

    /* This guarantees the correct version of youtube-dl is used. */
    if (!scriptOptions.isEmpty() &&
        !setOption("script-opts", scriptOptions))
    {
        filename.clear();
        goto cleanup;
    }

    initializeStatus = ::mpv_initialize(enc_h);
    if (initializeStatus < 0)
    {
        qWarning(
            "Could not initialize encoder: %s",
            ::mpv_error_string(initializeStatus)
        );
        filename = "";
        goto cleanup;
    }
    initialized = true;

    if (abort && abort->load())
    {
        filename.clear();
        ::mpv_request_log_messages(enc_h, "no");
        ::mpv_terminate_destroy(enc_h);
        enc_h = nullptr;
        goto cleanup;
    }

    commandStatus = ::mpv_command(enc_h, args);
    if (commandStatus < 0)
    {
        qWarning(
            "Could not encode file: %s",
            ::mpv_error_string(commandStatus)
        );
        filename = "";
        goto cleanup;
    }

    while (true)
    {
        if (abort && abort->load())
        {
            filename.clear();
            ::mpv_request_log_messages(enc_h, "no");
            ::mpv_terminate_destroy(enc_h);
            enc_h = nullptr;
            goto cleanup;
        }

        event = ::mpv_wait_event(enc_h, 100);
        if (event->event_id == MPV_EVENT_NONE)
        {
            continue;
        }
        if (event->event_id == MPV_EVENT_QUEUE_OVERFLOW)
        {
            qWarning("mpv encoder event queue overflowed");
            filename = "";
            goto cleanup;
        }
        if (event->event_id == MPV_EVENT_END_FILE)
        {
            const auto *endFile =
                static_cast<const mpv_event_end_file *>(event->data);
            if (endFile == nullptr)
            {
                qWarning("mpv encoder ended without a completion status");
                filename.clear();
            }
            else if (endFile->error < 0)
            {
                qWarning(
                    "mpv encoder failed: %s",
                    ::mpv_error_string(endFile->error)
                );
                filename.clear();
            }
            else if (endFile->reason == MPV_END_FILE_REASON_REDIRECT)
            {
                continue;
            }
            else if (endFile->reason != MPV_END_FILE_REASON_EOF)
            {
                qWarning(
                    "mpv encoder ended before EOF (reason=%d)",
                    endFile->reason
                );
                filename.clear();
            }
            ::mpv_terminate_destroy(enc_h);
            enc_h = nullptr;
            break;
        }
        if (event->event_id == MPV_EVENT_LOG_MESSAGE)
        {
            const auto *message =
                static_cast<const mpv_event_log_message *>(event->data);
            if (message != nullptr && !(abort && abort->load()))
            {
                const QString text = QString::fromUtf8(message->text).trimmed();
                if (!text.contains("client removed during hook handling"))
                {
                    qWarning(
                        "mpv encoder %s: %s",
                        message->prefix,
                        qUtf8Printable(text)
                    );
                }
            }
        }
        if (event->event_id == MPV_EVENT_SHUTDOWN)
        {
            qWarning("mpv encoder shut down before encoding completed");
            filename.clear();
            ::mpv_destroy(enc_h);
            enc_h = nullptr;
            goto cleanup;
        }
    }

cleanup:
    if (enc_h != nullptr)
    {
        if (initialized)
        {
            ::mpv_terminate_destroy(enc_h);
        }
        else
        {
            ::mpv_destroy(enc_h);
        }
    }

    QFile output(outputPath);
    if (abort && abort->load())
    {
        filename.clear();
    }
    if (filename.isEmpty() || !output.exists() || output.size() == 0)
    {
        if (!(abort && abort->load()))
        {
            qWarning(
                "mpv encoder produced no output at '%s' (exists=%d, size=%lld)",
                qUtf8Printable(outputPath),
                output.exists(),
                static_cast<long long>(output.size())
            );
        }
        if (output.exists())
        {
            output.remove();
        }
        filename.clear();
    }

    return filename;
}

const mpv_node *MpvController::mapValue(const mpv_node &node, const char *key)
{
    if (node.format != MPV_FORMAT_NODE_MAP || node.u.list == nullptr)
    {
        return nullptr;
    }

    mpv_node_list *list = node.u.list;
    for (int i = 0; i < list->num; ++i)
    {
        if (std::strcmp(list->keys[i], key) == 0)
        {
            return &list->values[i];
        }
    }
    return nullptr;
}

std::optional<int64_t> MpvController::mapInt(
    const mpv_node &node, const char *key)
{
    const mpv_node *value = mapValue(node, key);
    if (value == nullptr || value->format != MPV_FORMAT_INT64)
    {
        return std::nullopt;
    }
    return value->u.int64;
}

QString MpvController::mapString(const mpv_node &node, const char *key)
{
    const mpv_node *value = mapValue(node, key);
    if (value == nullptr || value->format != MPV_FORMAT_STRING)
    {
        return {};
    }
    return QString::fromUtf8(value->u.string);
}

const mpv_byte_array *MpvController::mapByteArray(
    const mpv_node &node, const char *key)
{
    const mpv_node *value = mapValue(node, key);
    if (value == nullptr || value->format != MPV_FORMAT_BYTE_ARRAY)
    {
        return nullptr;
    }
    return value->u.ba;
}

/* End Private Functions */
