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

#include <atomic>
#include <clocale>
#include <cstdio>
#include <functional>
#include <memory>

#include <QCoreApplication>
#include <QDir>
#include <QElapsedTimer>
#include <QEventLoop>
#include <QFile>
#include <QFutureWatcher>
#include <QGuiApplication>
#include <QMetaObject>
#include <QTemporaryFile>
#include <QThread>
#include <QTimer>
#include <QVariantMap>

#include "asr/asrcontroller.h"
#include "dict/dictionary.h"
#include "player/mpvcontroller.h"
#include "player/mpvplayer.h"
#include "player/mpvstate.h"
#include "state/context.h"
#include "util/directoryutils.h"

static std::atomic_int retryWarningCount{0};

/**
 * @brief Initialize and release Memento's dictionary DB around Context.
 */
class DictionaryDatabaseGuard final
{
public:
    bool initialize()
    {
        if (!QDir().mkpath(DirectoryUtils::getDataDir()))
        {
            return false;
        }
        Dictionary::createDatabaseInstance();
        return true;
    }

    ~DictionaryDatabaseGuard()
    {
        Dictionary::destroyDatabaseInstance();
    }
};

/**
 * @brief Count the controller's per-attempt retry warnings.
 *
 * @param type The Qt message type.
 * @param context The Qt message context.
 * @param message The emitted message.
 */
static void count_retry_warnings(
    QtMsgType type,
    const QMessageLogContext &context,
    const QString &message)
{
    Q_UNUSED(context)
    if (message.startsWith(QStringLiteral("ASR transcription failed")))
    {
        retryWarningCount.fetch_add(1);
    }
    if (type >= QtWarningMsg)
    {
        std::fprintf(stderr, "%s\n", qPrintable(message));
    }
}

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
 * @param timeoutMs The maximum wait in milliseconds.
 * @return true if the condition succeeds before the timeout.
 */
static bool wait_until(
    const std::function<bool()> &predicate,
    int timeoutMs = 5000)
{
    QElapsedTimer timer;
    timer.start();
    while (timer.elapsed() < timeoutMs)
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
 * @brief Run the controller decode-failure and playback-reactivation check.
 *
 * @param argc The argument count.
 * @param argv The generated media path is passed as the first argument.
 * @return EXIT_SUCCESS if terminal failure does not restart on position ticks.
 */
int main(int argc, char **argv)
{
    if (argc != 2)
    {
        std::fprintf(stderr, "usage: %s <media-file>\n", argv[0]);
        return 1;
    }

    QGuiApplication app(argc, argv);
    QCoreApplication::setOrganizationName("memento");
    QCoreApplication::setOrganizationDomain("ripose.projects");
    QCoreApplication::setApplicationName("memento");
    std::setlocale(LC_NUMERIC, "C");
    qInstallMessageHandler(count_retry_warnings);

    DictionaryDatabaseGuard dictionaryDatabase;
    if (!dictionaryDatabase.initialize())
    {
        std::fprintf(stderr, "Could not initialize the temporary test DB\n");
        return 1;
    }

    Context context;
    Settings *settings = context.settings();
    if (settings == nullptr)
    {
        std::fprintf(stderr, "Memento settings were not initialized\n");
        return 1;
    }

    QTemporaryFile invalidModel;
    if (!invalidModel.open())
    {
        std::fprintf(stderr, "Could not create the temporary invalid model\n");
        return 1;
    }
    const QString modelPath = invalidModel.fileName();
    invalidModel.close();

    settings->setAsrEnabled(true);
    settings->setWhisperModel(QStringLiteral("custom"));
    settings->setWhisperCustomModel(modelPath);
    settings->setWhisperUseGpu(false);
    settings->setWhisperFlashAttention(false);
    settings->setWhisperThreads(1);
    settings->setWhisperBestOf(1);
    settings->setWhisperBeamSize(1);

    TestMpvPlayer player;
    context.setPlayer(&player);
    if (!QMetaObject::invokeMethod(
            &player, "initializeMpv", Qt::DirectConnection) ||
        !player.optionsInitialized())
    {
        std::fprintf(stderr, "MpvPlayer initialization failed\n");
        return 1;
    }

    MpvController *mpvController = player.controller();
    if (mpvController == nullptr ||
        !mpvController->loadFile(QString::fromLocal8Bit(argv[1]), false) ||
        !wait_until([&player] { return player.isFileLoaded(); }))
    {
        std::fprintf(stderr, "Memento player setup failed\n");
        return 1;
    }

    MpvAudioClipArgs audioClipArgs;
    audioClipArgs.start = 0.0;
    audioClipArgs.end = player.state()->duration();
    audioClipArgs.preset = MpvAudioClipArgs::WhisperPcm16Wav;
    QFutureWatcher<QString> audioClipWatcher;
    audioClipWatcher.setFuture(
        mpvController->tempAudioClipAsync(audioClipArgs)
    );
    if (!wait_until([&audioClipWatcher]
    {
        return audioClipWatcher.isFinished();
    }, 10000))
    {
        std::fprintf(stderr, "Audio extraction probe did not complete\n");
        return 1;
    }

    const QString audioClipPath = audioClipWatcher.result();
    QFile audioClip(audioClipPath);
    const bool audioClipOpened = !audioClipPath.isEmpty() &&
        audioClip.open(QFile::ReadOnly);
    const QByteArray audioHeader = audioClipOpened ? audioClip.read(12) :
        QByteArray();
    const qint64 audioClipSize = audioClipOpened ? audioClip.size() : 0;
    const bool validWav = audioHeader.size() == 12 &&
        audioHeader.startsWith("RIFF") &&
        audioHeader.mid(8, 4) == QByteArrayLiteral("WAVE");
    audioClip.close();
    if (!audioClipPath.isEmpty())
    {
        QFile::remove(audioClipPath);
    }
    if (!validWav)
    {
        const QString syncPath = mpvController->tempAudioClip(audioClipArgs);
        QFile syncClip(syncPath);
        const bool syncOpened = !syncPath.isEmpty() &&
            syncClip.open(QFile::ReadOnly);
        const QByteArray syncHeader = syncOpened ? syncClip.read(12) :
            QByteArray();
        const qint64 syncSize = syncOpened ? syncClip.size() : 0;
        syncClip.close();
        if (!syncPath.isEmpty())
        {
            QFile::remove(syncPath);
        }
        std::fprintf(
            stderr,
            "Audio extraction probe failed: async-path='%s' opened=%d "
            "size=%lld header=%s sync-path='%s' sync-opened=%d "
            "sync-size=%lld sync-header=%s source='%s' aid=%lld "
            "duration=%.3f\n",
            qPrintable(audioClipPath),
            audioClipOpened,
            static_cast<long long>(audioClipSize),
            audioHeader.toHex().constData(),
            qPrintable(syncPath),
            syncOpened,
            static_cast<long long>(syncSize),
            syncHeader.toHex().constData(),
            qPrintable(player.state()->path()),
            static_cast<long long>(player.state()->aid()),
            player.state()->duration()
        );
        return 1;
    }

    QDir tempDirectory(QDir::tempPath());
    const QStringList filesBeforeCancel = tempDirectory.entryList(
        QDir::Files | QDir::NoDotAndDotDot,
        QDir::Name
    );
    const QStringList wavFilesBeforeCancel = tempDirectory.entryList(
        QStringList{QStringLiteral("*.wav")},
        QDir::Files,
        QDir::Name
    );
    auto extractionAbort = std::make_shared<std::atomic_bool>(false);
    QFutureWatcher<QString> canceledClipWatcher;
    canceledClipWatcher.setFuture(
        mpvController->tempAudioClipAsync(audioClipArgs, extractionAbort)
    );
    QStringList wavFilesDuringCancel;
    const bool cancellationOutputOpened = wait_until([&]
    {
        wavFilesDuringCancel = tempDirectory.entryList(
            QStringList{QStringLiteral("*.wav")},
            QDir::Files,
            QDir::Name
        );
        return wavFilesDuringCancel.size() > wavFilesBeforeCancel.size() ||
            canceledClipWatcher.isFinished();
    }, 5000);
    if (!cancellationOutputOpened || canceledClipWatcher.isFinished() ||
        wavFilesDuringCancel.size() <= wavFilesBeforeCancel.size())
    {
        extractionAbort->store(true);
        wait_until([&canceledClipWatcher]
        {
            return canceledClipWatcher.isFinished();
        }, 5000);
        std::fprintf(
            stderr,
            "Could not observe an active WAV output before cancellation\n"
        );
        return 1;
    }
    extractionAbort->store(true);
    if (!wait_until([&canceledClipWatcher]
    {
        return canceledClipWatcher.isFinished();
    }, 5000))
    {
        std::fprintf(stderr, "Canceled audio extraction did not finish\n");
        return 1;
    }
    const QString canceledClipPath = canceledClipWatcher.result();
    if (!canceledClipPath.isEmpty())
    {
        QFile::remove(canceledClipPath);
    }
    const QStringList filesAfterCancel = tempDirectory.entryList(
        QDir::Files | QDir::NoDotAndDotDot,
        QDir::Name
    );
    if (!canceledClipPath.isEmpty() || filesAfterCancel != filesBeforeCancel)
    {
        std::fprintf(
            stderr,
            "Canceled audio extraction left an output path or temporary file\n"
        );
        return 1;
    }

    AsrController asr(&context);
    int transcriptionStarts = 0;
    QObject::connect(
        &asr,
        &AsrController::runningChanged,
        &app,
        [&asr, &transcriptionStarts]
        {
            if (asr.running())
            {
                ++transcriptionStarts;
            }
        }
    );

    QCoro::QmlTask selection = asr.select(mpvController);
    QCoro::QmlTaskListener *selectionResult = selection.await();
    if (selectionResult == nullptr ||
        !wait_until([selectionResult]
        {
            return selectionResult->value().isValid();
        }))
    {
        std::fprintf(stderr, "ASR selection did not complete\n");
        return 1;
    }

    const QVariantMap result = selectionResult->value().toMap();
    const QString error = result.value("error").toString();
    const int initialRetryWarnings = retryWarningCount.load();
    const bool reportsFailureCause = error.contains(
        QStringLiteral("Whisper could not initialize the selected model")
    );
    if (result.value("fatal").toBool() ||
        !error.contains(QStringLiteral(
            "paused after repeated decode failures"
        )) ||
        initialRetryWarnings != 2 || transcriptionStarts != 1 || asr.running())
    {
        std::fprintf(
            stderr,
            "Expected one terminal controller failure; "
            "starts=%d warnings=%d error=%s\n",
            transcriptionStarts,
            initialRetryWarnings,
            qPrintable(error)
        );
        return 1;
    }

    int heartbeatCount = 0;
    int seekRequestCount = 0;
    QTimer heartbeat;
    heartbeat.setInterval(10);
    QObject::connect(
        &heartbeat,
        &QTimer::timeout,
        &app,
        [&heartbeatCount] { ++heartbeatCount; }
    );
    QObject::connect(
        mpvController,
        &MpvController::seekRequested,
        &app,
        [&seekRequestCount] { ++seekRequestCount; }
    );
    heartbeat.start();

    MpvState *state = player.state();
    const double initialPosition = state->timePosition();
    int positionUpdates = 0;
    QObject::connect(
        state,
        &MpvState::timePositionChanged,
        &app,
        [&positionUpdates, initialPosition](double position)
        {
            if (position > initialPosition + 0.01)
            {
                ++positionUpdates;
            }
        }
    );

    const int startsBeforePlayback = transcriptionStarts;
    const int warningsBeforePlayback = retryWarningCount.load();
    const int seekRequestsBeforePlayback = seekRequestCount;
    if (mpv_set_property_string(player.handle(), "speed", "4.0") < 0 ||
        mpv_set_property_string(player.handle(), "pause", "no") < 0)
    {
        std::fprintf(stderr, "Could not start high-speed test playback\n");
        return 1;
    }

    QThread::msleep(350);
    QElapsedTimer playbackTimer;
    playbackTimer.start();
    while (playbackTimer.elapsed() < 600)
    {
        QCoreApplication::processEvents(QEventLoop::AllEvents, 50);
        QThread::msleep(10);
    }

    const double finalPosition = state->timePosition();
    if (positionUpdates < 3 || finalPosition <= initialPosition + 1.0)
    {
        std::fprintf(
            stderr,
            "Delayed high-speed playback did not produce large position "
            "updates "
            "(count=%d, delta=%.3f)\n",
            positionUpdates,
            finalPosition - initialPosition
        );
        return 1;
    }

    if (mpv_set_property_string(player.handle(), "pause", "yes") < 0)
    {
        std::fprintf(stderr, "Could not pause before the explicit seek\n");
        return 1;
    }

    if (transcriptionStarts != startsBeforePlayback ||
        retryWarningCount.load() != warningsBeforePlayback ||
        seekRequestCount != seekRequestsBeforePlayback)
    {
        std::fprintf(
            stderr,
            "High-speed playback reactivated terminal ASR failure "
            "(starts=%d->%d, warnings=%d->%d, seek-requests=%d)\n",
            startsBeforePlayback,
            transcriptionStarts,
            warningsBeforePlayback,
            retryWarningCount.load(),
            seekRequestCount - seekRequestsBeforePlayback
        );
        return 1;
    }

    const int startsBeforeSeek = transcriptionStarts;
    const int warningsBeforeSeek = retryWarningCount.load();
    mpvController->seek(1.0);
    if (seekRequestCount != seekRequestsBeforePlayback + 1)
    {
        std::fprintf(
            stderr,
            "Explicit seek did not emit exactly one seek request "
            "(requests=%d->%d)\n",
            seekRequestsBeforePlayback,
            seekRequestCount
        );
        return 1;
    }
    if (!wait_until([&]
    {
        return transcriptionStarts > startsBeforeSeek &&
            !asr.running() &&
            retryWarningCount.load() >= warningsBeforeSeek + 2;
    }, 10000))
    {
        std::fprintf(
            stderr,
            "Explicit seek did not produce one bounded terminal retry "
            "(starts=%d->%d, warnings=%d->%d, running=%d)\n",
            startsBeforeSeek,
            transcriptionStarts,
            warningsBeforeSeek,
            retryWarningCount.load(),
            asr.running()
        );
        return 1;
    }
    asr.stop();
    if (transcriptionStarts != startsBeforeSeek + 1 ||
        retryWarningCount.load() != warningsBeforeSeek + 2)
    {
        std::fprintf(
            stderr,
            "Explicit seek retried more than once "
            "(starts=%d->%d, warnings=%d->%d)\n",
            startsBeforeSeek,
            transcriptionStarts,
            warningsBeforeSeek,
            retryWarningCount.load()
        );
        return 1;
    }
    if (heartbeatCount < 10)
    {
        std::fprintf(
            stderr,
            "Qt event loop did not remain responsive (heartbeat count=%d)\n",
            heartbeatCount
        );
        return 1;
    }
    if (!validWav)
    {
        std::fprintf(
            stderr,
            "Audio extractor produced a non-WAV clip "
            "(opened=%d, size=%lld, header=%s)\n",
            audioClipOpened,
            static_cast<long long>(audioClipSize),
            audioHeader.toHex().constData()
        );
        return 1;
    }
    if (!reportsFailureCause)
    {
        std::fprintf(
            stderr,
            "Controller omitted the decode cause from its result: %s\n",
            qPrintable(error)
        );
        return 1;
    }

    std::puts(
        "PASS: terminal failure ignores delayed high-speed playback and "
        "retries only after explicit seek; WAV cancellation cleans up"
    );
    return 0;
}
