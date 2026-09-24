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
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Memento.  If not, see <https://www.gnu.org/licenses/>.
//
////////////////////////////////////////////////////////////////////////////////

#include <clocale>
#include <cstdio>
#include <functional>

#include <QCoreApplication>
#include <QElapsedTimer>
#include <QEventLoop>
#include <QGuiApplication>
#include <QMetaObject>
#include <QThread>
#include <QVariantList>

#include "asr/asrbackend.h"
#include "player/mpvcontroller.h"
#include "player/mpvplayer.h"
#include "player/mpvsubtitle.h"

/**
 * @brief A deterministic ASR backend that emits one timed segment.
 */
class FakeAsrBackend final : public AsrBackend
{
public:
    QFuture<AsrTranscriptionResult> transcribe(
        const QString &audioPath,
        AsrTranscriptionOptions options) override
    {
        Q_UNUSED(audioPath)

        QPromise<AsrTranscriptionResult> promise;
        promise.start();
        if (options.abort != nullptr && options.abort->load())
        {
            promise.addResult(AsrTranscriptionResult::Canceled);
        }
        else
        {
            if (options.segmentCallback)
            {
                options.segmentCallback(SubtitleEntry{
                    .text = QStringLiteral("fake ASR segment"),
                    .start = 1.0,
                    .end = 2.5,
                });
            }
            promise.addResult(AsrTranscriptionResult::Success);
        }
        promise.finish();
        return promise.future();
    }
};

/**
 * @brief MpvPlayer configured for an offscreen integration test.
 */
class TestMpvPlayer final : public MpvPlayer
{
public:
    using MpvPlayer::MpvPlayer;

    void initOptions() override
    {
        m_optionsInitialized =
            setOption("config", "no") &&
            setOption("terminal", "no") &&
            setOption("vo", "null") &&
            setOption("ao", "null") &&
            setOption("pause", "yes");
    }

    bool optionsInitialized() const noexcept
    {
        return m_optionsInitialized;
    }

private:
    bool setOption(const char *name, const char *value) const
    {
        return mpv_set_option_string(handle(), name, value) >= 0;
    }

    bool m_optionsInitialized{false};
};

/**
 * @brief Process Qt events until a condition succeeds or times out.
 *
 * @param predicate The condition to check.
 * @return true if the condition succeeds within five seconds.
 */
static bool wait_until(const std::function<bool()> &predicate)
{
    QElapsedTimer timer;
    timer.start();
    while (timer.elapsed() < 5000)
    {
        QCoreApplication::processEvents(QEventLoop::AllEvents, 50);
        if (predicate())
        {
            return true;
        }
        QThread::msleep(10);
    }
    return predicate();
}

/**
 * @brief Check that the latest emitted line matches its expected values.
 *
 * @param lines The latest primary-subtitle-lines property value.
 * @param cue_id The expected cue ID.
 * @param text The expected cue text.
 * @return true if the single line matches.
 */
static bool line_matches(
    const QVariantList &lines, qint64 cue_id, const QString &text)
{
    if (lines.size() != 1)
    {
        return false;
    }

    const QVariantMap line = lines.first().toMap();
    return line.value("id").toLongLong() == cue_id &&
        line.value("text").toString() == text &&
        qAbs(line.value("start").toDouble() - 1.0) < 0.0001 &&
        qAbs(line.value("end").toDouble() - 2.5) < 0.0001;
}

/**
 * @brief Run the fake-ASR-to-libmpv integration check.
 *
 * @param argc The argument count.
 * @param argv The generated media path is passed as the first argument.
 * @return EXIT_SUCCESS if all checks pass.
 */
int main(int argc, char **argv)
{
    if (argc != 2)
    {
        std::fprintf(stderr, "usage: %s <media-file>\n", argv[0]);
        return 1;
    }

    QGuiApplication app(argc, argv);
    std::setlocale(LC_NUMERIC, "C");
    TestMpvPlayer player;
    if (!QMetaObject::invokeMethod(
            &player, "initializeMpv", Qt::DirectConnection) ||
        !player.optionsInitialized())
    {
        std::fprintf(stderr, "MpvPlayer initialization failed\n");
        return 1;
    }

    MpvController *controller = player.controller();
    if (controller == nullptr || !controller->supportsMemorySubtitles() ||
        !controller->loadFile(QString::fromLocal8Bit(argv[1]), false) ||
        !wait_until([&player] { return player.isFileLoaded(); }))
    {
        std::fprintf(stderr, "Memento player setup failed\n");
        return 1;
    }

    QVariantList lines;
    QObject::connect(
        &player,
        &MpvPlayer::primarySubtitleLinesChanged,
        &app,
        [&lines](const QVariantList &value) { lines = value; }
    );

    const qint64 track_id = controller->createSubtitleTrack(
        QStringLiteral("ASR test"), QStringLiteral("ja")
    );
    if (track_id <= 0)
    {
        std::fprintf(stderr, "MpvController failed to create a track\n");
        return 1;
    }

    qint64 cue_id = -1;
    AsrTranscriptionOptions options;
    options.segmentCallback =
        [&controller, track_id, &cue_id](const SubtitleEntry &segment)
        {
            cue_id = controller->addSubtitle(
                track_id,
                segment.start,
                segment.end,
                segment.text
            );
        };

    FakeAsrBackend backend;
    const AsrTranscriptionResult result =
        backend.transcribe(QString(), std::move(options)).result();
    if (result != AsrTranscriptionResult::Success || cue_id <= 0 ||
        !wait_until([&lines, cue_id]
        {
            return line_matches(
                lines, cue_id, QStringLiteral("fake ASR segment")
            );
        }))
    {
        std::fprintf(stderr, "Fake ASR segment did not reach sub-lines\n");
        return 1;
    }

    if (mpv_set_property_string(player.handle(), "sub-speed", "0.5") < 0 ||
        mpv_set_property_string(player.handle(), "sub-delay", "0.25") < 0 ||
        !wait_until([&player]
        {
            MpvSubtitle *subtitle = player.state()->subtitle();
            return subtitle->speed() == 0.5 && subtitle->delay() == 0.25;
        }))
    {
        std::fprintf(stderr, "Subtitle timing properties did not update\n");
        return 1;
    }

    controller->seek(1.0);
    if (!wait_until([&player]
        {
            MpvSubtitle *subtitle = player.state()->subtitle();
            return subtitle->text() == QStringLiteral("fake ASR segment") &&
                qAbs(player.state()->timePosition() - 1.0) < 0.05;
        }))
    {
        std::fprintf(stderr, "Scaled, delayed cue was not selected\n");
        return 1;
    }

    MpvSubtitle *subtitle = player.state()->subtitle();
    if (qAbs(subtitle->rawTime(player.state()->timePosition()) - 1.5) >
            0.05 ||
        qAbs(subtitle->playbackTime(subtitle->startTime()) - 0.75) >
            0.0001)
    {
        std::fprintf(stderr, "Subtitle timeline conversion is incorrect\n");
        return 1;
    }

    if (!controller->editSubtitle(
            track_id, cue_id, 1.0, 2.5, QStringLiteral("edited segment")) ||
        !wait_until([&lines, cue_id]
        {
            return line_matches(
                lines, cue_id, QStringLiteral("edited segment")
            );
        }) ||
        controller->addSubtitle(
            track_id, -1.0, 2.5, QStringLiteral("invalid segment")) >= 0 ||
        !controller->removeSubtitle(track_id, cue_id) ||
        !wait_until([&lines] { return lines.isEmpty(); }) ||
        !controller->removeSubtitleTrack(track_id))
    {
        std::fprintf(stderr, "Memento ASR subtitle lifecycle failed\n");
        return 1;
    }

    std::puts("PASS: fake ASR segment through MpvController and sub-lines");
    return 0;
}
