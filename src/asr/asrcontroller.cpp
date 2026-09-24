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

#include "asr/asrcontroller.h"

#ifdef MEMENTO_WHISPER_SUPPORT
#define MEMENTO_ASR_BACKEND_SUPPORT
#endif // MEMENTO_WHISPER_SUPPORT

#include <algorithm>
#include <cmath>
#include <limits>
#include <utility>

#include <QDir>
#include <QElapsedTimer>
#include <QFile>
#include <QFileInfo>
#include <QMetaObject>
#include <QMutex>
#include <QMutexLocker>
#include <QPointer>
#include <QUrl>
#include <QVariantMap>

#ifdef MEMENTO_SYSTEM_QCORO
#include <QCoroFuture>
#else
#include <qcoro/core/qcorofuture.h>
#endif // MEMENTO_SYSTEM_QCORO

#include "player/mpvcontroller.h"
#include "player/mpvplayer.h"
#include "player/mpvstate.h"
#include "setting/keys.h"
#include "setting/settings.h"
#include "state/context.h"
#include "manager/downloadmanager.h"
#include "asr/whisper/whispercontroller.h"

static constexpr const char *KEY_ERROR = "error";
static constexpr const char *MODEL_SUFFIX = ".bin";

static constexpr double WORK_WINDOW_SECONDS = 30.0;
static constexpr double MIN_WORK_WINDOW_SECONDS = 0.25;
static constexpr double SEEK_RESTART_THRESHOLD_SECONDS = 5.0;
static constexpr double SEEK_RESTART_CLOCK_SLOP_SECONDS = 2.0;
static constexpr double SUBTITLE_TIME_DELTA = 0.0001;
static constexpr int MAX_TRANSCRIPTION_FAILURES = 3;

AsrController::AsrController(Context *context, QObject *parent) :
    QObject(parent),
    m_context(context),
    m_downloader(context != nullptr ? context->downloadManager() : nullptr),
    m_whisperController(new WhisperController(context, this))
{
    if (m_context != nullptr)
    {
        if (Settings *settings = m_context->settings())
        {
            connect(
                settings,
                &Settings::asrEnabledChanged,
                this,
                &AsrController::requestReconfigure
            );
            connect(
                settings,
                &Settings::asrBackendChanged,
                this,
                &AsrController::requestReconfigure
            );
        }

        connect(
            m_whisperController,
            &WhisperController::configurationChanged,
            this,
            &AsrController::requestReconfigure
        );
    }

    if (m_downloader != nullptr)
    {
        connect(m_downloader, &DownloadManager::downloadRunningChanged, this, &AsrController::downloadRunningChanged);
        connect(m_downloader, &DownloadManager::downloadProgressChanged, this, &AsrController::downloadProgressChanged);
        connect(m_downloader, &DownloadManager::downloadNameChanged, this, &AsrController::downloadNameChanged);
    }
}

AsrController::~AsrController()
{
    stop();
}

bool AsrController::active() const noexcept
{
    return m_active;
}

bool AsrController::running() const noexcept
{
    return m_running;
}

QString AsrController::currentText() const
{
    return m_currentText;
}

qint64 AsrController::subtitleTrackId() const noexcept
{
    return m_subtitleTrackId;
}

bool AsrController::downloadRunning() const noexcept
{
    return m_downloader->downloadRunning();
}

qreal AsrController::downloadProgress() const noexcept
{
    return m_downloader->downloadProgress();
}

qint64 AsrController::downloadReceived() const noexcept
{
    return m_downloader->downloadReceived();
}

qint64 AsrController::downloadTotal() const noexcept
{
    return m_downloader->downloadTotal();
}

qint64 AsrController::downloadSpeed() const noexcept
{
    return m_downloader->downloadSpeed();
}

QString AsrController::downloadName() const
{
    return m_downloader->downloadName();
}

QCoro::QmlTask AsrController::select(MpvController *controller)
{
    m_retryPaused = false;
    m_retryAwaitingSeekPosition = false;
    return selectAsync(controller);
}

void AsrController::stop()
{
    ++m_selectGeneration;
    ++m_generation;
    ++m_segmentGeneration;
    m_restartRequested = false;
    m_reconfigureRequested = false;
    m_isReconfiguring = false;
    m_retryPaused = false;
    m_retryAwaitingSeekPosition = false;

    if (m_abort)
    {
        m_abort->store(true);
    }

    if (m_positionConnection)
    {
        disconnect(m_positionConnection);
    }
    if (m_pathConnection)
    {
        disconnect(m_pathConnection);
    }
    if (m_sidConnection)
    {
        disconnect(m_sidConnection);
    }
    if (m_seekConnection)
    {
        disconnect(m_seekConnection);
    }
    if (m_subtitleTextConnection)
    {
        disconnect(m_subtitleTextConnection);
    }
    if (m_fileStartedConnection)
    {
        disconnect(m_fileStartedConnection);
    }
    if (m_fileEndedConnection)
    {
        disconnect(m_fileEndedConnection);
    }

    discardSubtitleTrack(true);
    setActive(false);
    setRunning(false);
    setCurrentText({});
    m_controller = nullptr;
}

QString AsrController::modelsDirectory() const
{
    return m_whisperController->modelsDirectory();
}

bool AsrController::hasAnyModel() const
{
    Settings *settings = m_context != nullptr ? m_context->settings() : nullptr;
    const QString backend = settings != nullptr
        ? settings->asrBackend()
        : QString::fromLatin1(Keys::Asr::BACKEND_WHISPER);
    if (backend == Keys::Asr::BACKEND_WHISPER)
    {
        return m_whisperController != nullptr ? m_whisperController->hasAnyModel() : false;
    }
    return false;
}

bool AsrController::modelAvailable(const QString &model) const
{
    return m_whisperController->modelAvailable(model);
}

bool AsrController::selectedModelAvailable() const
{
    const QString path = selectedModelPath();
    return !path.isEmpty() && QFileInfo::exists(path);
}

bool AsrController::selectedModelDownloadable() const
{
    if (m_context == nullptr || m_context->settings() == nullptr)
    {
        return false;
    }

    Settings *settings = m_context->settings();
    if (settings->asrBackend() == Keys::Asr::BACKEND_WHISPER)
    {
        return m_whisperController->isManagedModel(settings->whisperModel());
    }
    return false;
}

QCoro::QmlTask AsrController::downloadModel(const QString &model)
{
    return downloadModelAsync(model);
}

QCoro::Task<QVariantMap> AsrController::downloadModelAsync(QString model)
{
    QVariantMap result;

#ifndef MEMENTO_ASR_BACKEND_SUPPORT
    Q_UNUSED(model)
    result[KEY_ERROR] = tr(
        "ASR subtitle support is not available in this build.");
    co_return result;
#else
    if (m_downloader->downloadRunning())
    {
        result[KEY_ERROR] = tr("An ASR model is already downloading.");
        co_return result;
    }

    QString path;
    QUrl url;
    bool reconfigureWhenDone = false;
    Settings *settings = m_context->settings();

    if (m_whisperController->isManagedModel(model))
    {
#ifndef MEMENTO_WHISPER_SUPPORT
        result[KEY_ERROR] = tr(
            "Whisper subtitle support is not available in this build.");
        co_return result;
#else
        path = m_whisperController->resolveModelPath(
            m_whisperController->modelFilename(model),
            QStringList{MODEL_SUFFIX}
        );
        url = m_whisperController->whisperModelUrl(model);
        reconfigureWhenDone =
            m_context != nullptr &&
            settings != nullptr &&
            settings->asrBackend() ==
                Keys::Asr::BACKEND_WHISPER &&
            settings->whisperModel() == model;
#endif // MEMENTO_WHISPER_SUPPORT
    }

    else
    {
        result[KEY_ERROR] = tr("This ASR model cannot be downloaded.");
        co_return result;
    }

    if (QFileInfo::exists(path))
    {
        const qint64 size = QFileInfo(path).size();
        m_downloader->setDownloadName(model);
        m_downloader->setDownloadProgress(size, size, 0);
        result["path"] = path;
        if (reconfigureWhenDone)
        {
            requestReconfigure();
        }
        co_return result;
    }

    m_downloader->setDownloadName(model);
    m_downloader->setDownloadProgress(0, 0, 0);
    m_downloader->setDownloadRunning(true);
    const bool ok = co_await m_downloader->download(
        url,
        path,
        model,
        true
    );
    m_downloader->setDownloadRunning(false);

    if (!ok)
    {
        result[KEY_ERROR] = tr("Could not download ASR model: %1")
            .arg(model);
        co_return result;
    }

    result["path"] = path;
    if (reconfigureWhenDone)
    {
        requestReconfigure();
    }
    co_return result;
#endif // MEMENTO_ASR_BACKEND_SUPPORT
}

QCoro::Task<QVariantMap> AsrController::selectAsync(
    MpvController *controller)
{
    QVariantMap result;
    if (m_selectInFlight)
    {
        result[KEY_ERROR] = tr("ASR is already transcribing.");
        co_return result;
    }

    m_selectInFlight = true;
    QPointer<AsrController> self(this);
    result = co_await selectImplAsync(controller);
    if (self == nullptr)
    {
        co_return result;
    }
    m_selectInFlight = false;
    if (m_reconfigureRequested)
    {
        QMetaObject::invokeMethod(
            this,
            &AsrController::restartAfterReconfigure,
            Qt::QueuedConnection
        );
    }
    co_return result;
}

QCoro::Task<QVariantMap> AsrController::selectImplAsync(
    MpvController *controller)
{
    QVariantMap result;

#ifndef MEMENTO_ASR_BACKEND_SUPPORT
    Q_UNUSED(controller)
    result[KEY_ERROR] = tr(
        "ASR subtitle support is not available in this build.");
    co_return result;
#else
    QPointer<AsrController> self(this);
    if (m_running)
    {
        result[KEY_ERROR] = tr("ASR is already transcribing.");
        co_return result;
    }

    if (m_context == nullptr ||
        m_context->settings() == nullptr ||
        !m_context->settings()->asrEnabled())
    {
        result[KEY_ERROR] = tr("ASR subtitles are disabled.");
        co_return result;
    }

    if (controller == nullptr ||
        controller->player() == nullptr ||
        controller->player()->state() == nullptr)
    {
        result[KEY_ERROR] = tr("No player is available.");
        co_return result;
    }

    MpvState *state = controller->player()->state();
    if (state->path().isEmpty() || state->duration() <= 0.0 ||
        !controller->player()->isFileLoaded())
    {
        result[KEY_ERROR] = tr("No media is loaded.");
        co_return result;
    }

    Settings *settings = m_context->settings();
    const QString backendName = settings->asrBackend();
    if (backendName == Keys::Asr::BACKEND_WHISPER)
    {
#ifndef MEMENTO_WHISPER_SUPPORT
        result[KEY_ERROR] = tr(
            "Whisper subtitle support is not available in this build.");
        co_return result;
#endif // MEMENTO_WHISPER_SUPPORT
    }

    else
    {
        result[KEY_ERROR] = tr("The selected ASR backend is not available.");
        co_return result;
    }

    const bool isReconfigure = m_isReconfiguring.exchange(false) ||
        m_reconfigureRequested.exchange(false);

    const int selectGeneration = ++m_selectGeneration;
    setRunning(true);
    const QString modelPath = selectedModelPath();
    if (modelPath.isEmpty() || !QFileInfo::exists(modelPath))
    {
        setRunning(false);
        result["missingModel"] = true;
        result["path"] = modelPath;
        result[KEY_ERROR] = tr("ASR model was not found: %1")
            .arg(modelPath);
        co_return result;
    }

    const QString mediaPath = state->path();
    m_restartRequested = false;
    if (!controller->supportsMemorySubtitles())
    {
        setRunning(false);
        result[KEY_ERROR] = tr(
            "In-memory subtitles are not supported by the active player.");
        co_return result;
    }

    QPointer<MpvController> controllerGuard(controller);
    QPointer<MpvPlayer> playerGuard(controller->player());
    QPointer<MpvState> stateGuard(state);
    watchMediaLifecycle(controller, mediaPath);

    if (!m_backend)
    {
        WhisperBackendCreationResult backendResult = co_await
            m_whisperController->createBackend(modelPath);
        if (self == nullptr)
        {
            co_return result;
        }
        if (selectGeneration != m_selectGeneration)
        {
            co_return result;
        }
        if (controllerGuard == nullptr || playerGuard == nullptr ||
            stateGuard == nullptr)
        {
            stop();
            result[KEY_ERROR] = tr("No player is available.");
            co_return result;
        }
        if (m_reconfigureRequested)
        {
            m_reconfigureRequested = false;
            m_isReconfiguring = true;
            setRunning(false);
            co_return co_await selectImplAsync(controller);
        }
        if (!backendResult.backend)
        {
            setRunning(false);
            result[KEY_ERROR] = backendResult.error.isEmpty() ?
                tr("Could not instantiate ASR backend.") :
                backendResult.error;
            co_return result;
        }
        m_backend = std::move(backendResult.backend);
    }
    if (controllerGuard == nullptr || playerGuard == nullptr ||
        stateGuard == nullptr || !playerGuard->isFileLoaded() ||
        state->path() != mediaPath || state->duration() <= 0.0)
    {
        setRunning(false);
        result[KEY_ERROR] = tr("Media changed while ASR was starting.");
        co_return result;
    }

    if (!ensureSubtitleTrack(controller, mediaPath, result))
    {
        setRunning(false);
        co_return result;
    }
    const std::shared_ptr<AsrBackend> currentBackend = m_backend;
    if (currentBackend == nullptr)
    {
        setRunning(false);
        result[KEY_ERROR] = tr("Could not instantiate ASR backend.");
        co_return result;
    }

    if (isReconfigure)
    {
        invalidateSubtitleCues();
    }

    const int generation = ++m_generation;
    ++m_segmentGeneration;
    m_restartRequested = false;
    m_controller = controller;
    m_lastPosition = state->timePosition();
    m_lastPositionTimer.restart();

    if (m_positionConnection)
    {
        disconnect(m_positionConnection);
    }
    m_positionConnection = connect(
        state,
        &MpvState::timePositionChanged,
        this,
        [this] (double position) { handlePositionChanged(position); }
    );

    setActive(true);
    setCurrentText({});

    const size_t initialCueCount = m_subtitleCues.size();
    double restartPosition = 0.0;
    double start = consumeRestart(restartPosition)
        ? restartPosition
        : qBound(0.0, state->timePosition(), state->duration());

    if (start >= state->duration() - MIN_WORK_WINDOW_SECONDS)
    {
        start = 0.0;
    }
    start = skipCoveredPosition(start);
    bool failed = false;
    bool fatalFailure = true;
    int consecutiveFailures = 0;
    bool rewound = false;

    while (generation == m_generation && m_active && !m_reconfigureRequested)
    {
        if (m_controller == nullptr)
        {
            failed = true;
            result[KEY_ERROR] = tr("No player is available.");
            break;
        }

        const double duration = state->duration();
        if (state->path() != mediaPath || duration <= 0.0)
        {
            failed = true;
            result[KEY_ERROR] = tr("Media changed while ASR was running.");
            break;
        }

        start = qBound(0.0, skipCoveredPosition(start), duration);
        if (start >= duration - MIN_WORK_WINDOW_SECONDS)
        {
            if (!rewound && skipCoveredPosition(0.0) < duration - MIN_WORK_WINDOW_SECONDS)
            {
                rewound = true;
                start = skipCoveredPosition(0.0);
                continue;
            }
            break;
        }

        const double nextStart = nextSubtitleStart(start);
        double end = qMin(duration, start + WORK_WINDOW_SECONDS);
        if (std::isfinite(nextStart))
        {
            end = qMin(end, nextStart);
        }
        if (end <= start + MIN_WORK_WINDOW_SECONDS)
        {
            start = qMin(duration, start + MIN_WORK_WINDOW_SECONDS);
            continue;
        }

        m_abort = std::make_shared<std::atomic_bool>(false);
        const std::shared_ptr<std::atomic_bool> attemptAbort = m_abort;
        const int segmentGeneration = m_segmentGeneration;

        MpvAudioClipArgs args;
        args.start = start;
        args.end = end;
        args.normalize = false;
        args.preset = MpvAudioClipArgs::WhisperPcm16Wav;

        QElapsedTimer extractionTimer;
        extractionTimer.start();
        const QString audioPath = co_await qCoro(
            m_controller->tempAudioClipAsync(args, attemptAbort)
        ).takeResult();
        qInfo(
            "ASR_STAGE_PERF extraction_await_ms=%lld",
            static_cast<long long>(extractionTimer.elapsed())
        );
        if (self == nullptr)
        {
            QFile::remove(audioPath);
            co_return result;
        }
        if (generation != m_generation || !m_active)
        {
            QFile::remove(audioPath);
            co_return result;
        }
        if (audioPath.isEmpty())
        {
            failed = true;
            result[KEY_ERROR] = tr("Could not extract audio for ASR.");
            break;
        }

        double restartPosition = 0.0;
        if (m_reconfigureRequested)
        {
            QFile::remove(audioPath);
            break;
        }
        if (consumeRestart(restartPosition))
        {
            QFile::remove(audioPath);
            start = restartPosition;
            rewound = false;
            continue;
        }

        AsrTranscriptionOptions options{
            .language = QStringLiteral("ja"),
            .abort = m_abort,
            .segmentCallback = {},
            .failureCallback = {},
        };
        auto failureMessage = std::make_shared<QString>();
        auto failureMutex = std::make_shared<QMutex>();
        options.failureCallback = [failureMessage, failureMutex]
            (const QString &message)
            {
                QMutexLocker lock(failureMutex.get());
                *failureMessage = message;
            };

        QPointer<AsrController> callbackSelf(this);
        options.segmentCallback =
            [callbackSelf, generation, segmentGeneration, start, end]
            (const SubtitleEntry &subtitle)
            {
                if (callbackSelf == nullptr)
                {
                    return;
                }

                SubtitleEntry adjusted = subtitle;
                adjusted.start = qBound(start, adjusted.start + start, end);
                adjusted.end = qBound(start, adjusted.end + start, end);
                QMetaObject::invokeMethod(
                    callbackSelf,
                    [callbackSelf, generation, segmentGeneration, adjusted]
                    {
                        if (callbackSelf != nullptr)
                        {
                            callbackSelf->addSubtitle(
                                generation,
                                segmentGeneration,
                                adjusted
                            );
                        }
                    },
                    Qt::QueuedConnection
                );
            };

        QElapsedTimer inferenceTimer;
        inferenceTimer.start();
        AsrTranscriptionResult transcribeResult =
            co_await qCoro(
                currentBackend->transcribe(
                    audioPath,
                    options
                )
            ).takeResult();
        qInfo(
            "ASR_STAGE_PERF inference_ms=%lld",
            static_cast<long long>(inferenceTimer.elapsed())
        );

        if (self == nullptr)
        {
            QFile::remove(audioPath);
            co_return result;
        }
        QFile::remove(audioPath);
        if (controllerGuard == nullptr || playerGuard == nullptr ||
            stateGuard == nullptr)
        {
            stop();
            result[KEY_ERROR] = tr("No player is available.");
            co_return result;
        }

        if (m_reconfigureRequested)
        {
            break;
        }
        if (consumeRestart(restartPosition))
        {
            start = restartPosition;
            rewound = false;
            continue;
        }
        if (generation != m_generation || !m_active)
        {
            if (m_abort == attemptAbort)
            {
                m_abort.reset();
            }
            co_return result;
        }
        if (transcribeResult == AsrTranscriptionResult::Canceled)
        {
            start = qBound(0.0, state->timePosition(), duration);
            continue;
        }
        if (transcribeResult != AsrTranscriptionResult::Success)
        {
            ++consecutiveFailures;
            QString failureDetail;
            {
                QMutexLocker lock(failureMutex.get());
                failureDetail = *failureMessage;
            }
            if (consecutiveFailures >= MAX_TRANSCRIPTION_FAILURES)
            {
                failed = true;
                fatalFailure = false;
                m_retryPaused = true;
                m_retryAwaitingSeekPosition = false;
                result["fatal"] = false;
                result[KEY_ERROR] = failureDetail.isEmpty() ? tr(
                    "ASR transcription paused after repeated decode "
                    "failures. Seek or change ASR settings to retry."
                ) : tr(
                    "ASR transcription paused after repeated decode "
                    "failures: %1. Seek or change ASR settings to retry."
                ).arg(failureDetail);
                break;
            }

            if (failureDetail.isEmpty())
            {
                qWarning(
                    "ASR transcription failed; retrying from the current "
                    "playback position."
                );
            }
            else
            {
                qWarning(
                    "ASR transcription failed (%s); retrying from the "
                    "current playback position.",
                    qUtf8Printable(failureDetail)
                );
            }
            start = qBound(0.0, state->timePosition(), duration);
            continue;
        }

        consecutiveFailures = 0;
        start = end;
    }

    if (generation != m_generation || !m_active)
    {
        co_return result;
    }

    m_abort.reset();
    setRunning(false);
    updateCurrentText();

    if (generation != m_generation || !m_active)
    {
        co_return result;
    }
    if (m_reconfigureRequested.exchange(false))
    {
        if (m_controller != nullptr)
        {
            m_backend.reset();
            m_isReconfiguring = true;
            MpvController *controller = m_controller.data();
            co_return co_await selectImplAsync(controller);
        }
        result[KEY_ERROR] = tr("No player is available.");
        co_return result;
    }
    if (failed)
    {
        if (fatalFailure)
        {
            stop();
        }
        co_return result;
    }
    if (initialCueCount == 0 && m_subtitleCues.empty())
    {
        stop();
        result[KEY_ERROR] = tr("No speech was detected.");
        co_return result;
    }

    co_return result;
#endif // MEMENTO_ASR_BACKEND_SUPPORT
}

void AsrController::setActive(bool value)
{
    if (m_active == value)
    {
        return;
    }
    m_active = value;
    emit activeChanged();
}

void AsrController::setRunning(bool value)
{
    if (m_running == value)
    {
        return;
    }
    m_running = value;
    emit runningChanged();
}

void AsrController::setCurrentText(QString value)
{
    if (m_currentText == value)
    {
        return;
    }
    m_currentText = std::move(value);
    emit currentTextChanged();
}

void AsrController::setSubtitleTrackId(qint64 value)
{
    if (m_subtitleTrackId == value)
    {
        return;
    }
    m_subtitleTrackId = value;
    emit subtitleTrackIdChanged();
}

bool AsrController::ensureSubtitleTrack(
    MpvController *controller,
    const QString &mediaPath,
    QVariantMap &result)
{
    MpvPlayer *player = controller == nullptr ? nullptr : controller->player();
    MpvState *state = player == nullptr ? nullptr : player->state();
    if (controller == nullptr || state == nullptr ||
        !player->isFileLoaded() || !controller->supportsMemorySubtitles())
    {
        result[KEY_ERROR] = tr(
            "The active player cannot create an in-memory subtitle track.");
        return false;
    }

    if (m_subtitleTrackId >= 0 &&
        m_controller == controller &&
        m_subtitleTrackMediaPath == mediaPath)
    {
        return true;
    }

    if (m_positionConnection)
    {
        disconnect(m_positionConnection);
    }
    if (m_sidConnection)
    {
        disconnect(m_sidConnection);
    }
    if (m_subtitleTextConnection)
    {
        disconnect(m_subtitleTextConnection);
    }
    if (m_subtitleTrackId >= 0)
    {
        discardSubtitleTrack(true);
    }

    const qint64 previousSid = state->sid();
    const qint64 trackId = controller->createSubtitleTrack(
        tr("ASR subtitles"),
        QStringLiteral("ja")
    );
    if (trackId < 0)
    {
        result[KEY_ERROR] = tr("Could not create an in-memory subtitle track.");
        return false;
    }

    m_controller = controller;
    setSubtitleTrackId(trackId);
    m_previousSubtitleSid = previousSid;
    m_subtitleTrackMediaPath = mediaPath;
    m_sidConnection = connect(
        state,
        &MpvState::sidChanged,
        this,
        &AsrController::handleSidChanged
    );
    if (state->subtitle() != nullptr)
    {
        m_subtitleTextConnection = connect(
            state->subtitle(),
            &MpvSubtitle::textChanged,
            this,
            &AsrController::updateCurrentText
        );
    }
    return true;
}

void AsrController::watchMediaLifecycle(
    MpvController *controller,
    const QString &mediaPath)
{
    MpvPlayer *player = controller == nullptr ? nullptr : controller->player();
    MpvState *state = player == nullptr ? nullptr : player->state();
    if (controller == nullptr || player == nullptr || state == nullptr)
    {
        return;
    }

    const bool controllerChanged = m_controller != controller;
    const bool trackChanged = m_subtitleTrackId >= 0 &&
        (controllerChanged || m_subtitleTrackMediaPath != mediaPath);
    if (trackChanged)
    {
        if (m_positionConnection)
        {
            disconnect(m_positionConnection);
        }
        if (m_sidConnection)
        {
            disconnect(m_sidConnection);
        }
        if (m_seekConnection)
        {
            disconnect(m_seekConnection);
        }
        if (m_subtitleTextConnection)
        {
            disconnect(m_subtitleTextConnection);
        }
        discardSubtitleTrack(true);
    }
    else if (controllerChanged)
    {
        if (m_positionConnection)
        {
            disconnect(m_positionConnection);
        }
        if (m_sidConnection)
        {
            disconnect(m_sidConnection);
        }
        if (m_seekConnection)
        {
            disconnect(m_seekConnection);
        }
        if (m_subtitleTextConnection)
        {
            disconnect(m_subtitleTextConnection);
        }
    }

    if (m_pathConnection)
    {
        disconnect(m_pathConnection);
    }
    if (m_fileStartedConnection)
    {
        disconnect(m_fileStartedConnection);
    }
    if (m_fileEndedConnection)
    {
        disconnect(m_fileEndedConnection);
    }
    if (m_seekConnection)
    {
        disconnect(m_seekConnection);
    }

    m_controller = controller;
    m_subtitleTrackMediaPath = mediaPath;
    m_pathConnection = connect(
        state,
        &MpvState::pathChanged,
        this,
        &AsrController::handleMediaPathChanged
    );
    m_fileStartedConnection = connect(
        player,
        &MpvPlayer::fileStarted,
        this,
        &AsrController::handleFileLifecycleChange
    );
    m_fileEndedConnection = connect(
        player,
        &MpvPlayer::fileEnded,
        this,
        &AsrController::handleFileLifecycleChange
    );
    m_seekConnection = connect(
        controller,
        &MpvController::seekRequested,
        this,
        &AsrController::handleSeeked
    );
}

void AsrController::invalidateSubtitleCues()
{
    m_subtitleCues.clear();
    setCurrentText({});
    if (m_subtitleTrackId < 0)
    {
        return;
    }

    if (m_controller == nullptr ||
        !m_controller->clearSubtitles(m_subtitleTrackId))
    {
        discardSubtitleTrack(true);
    }
}

void AsrController::discardSubtitleTrack(bool removeFromPlayer)
{
    MpvController *controller = m_controller.data();
    MpvPlayer *player = controller == nullptr ? nullptr : controller->player();
    MpvState *state = player == nullptr ? nullptr : player->state();
    const qint64 trackId = m_subtitleTrackId;
    const qint64 currentSid = state == nullptr ? 0 : state->sid();
    const bool sameMedia = state != nullptr &&
        player->isFileLoaded() &&
        state->duration() > 0.0 &&
        state->path() == m_subtitleTrackMediaPath;

    if (removeFromPlayer && trackId >= 0 && sameMedia)
    {
        if (!controller->removeSubtitleTrack(trackId))
        {
            qWarning("Could not remove the generated ASR subtitle track.");
        }

        bool previousTrackExists = m_previousSubtitleSid == 0;
        if (!previousTrackExists)
        {
            for (const MpvTrack *track : state->subtitleTracks())
            {
                if (track != nullptr && track->id() == m_previousSubtitleSid)
                {
                    previousTrackExists = true;
                    break;
                }
            }
        }
        if (currentSid == trackId && previousTrackExists)
        {
            controller->setSid(m_previousSubtitleSid);
        }
    }

    setSubtitleTrackId(-1);
    m_previousSubtitleSid = 0;
    m_subtitleTrackMediaPath.clear();
    m_subtitleCues.clear();
}

void AsrController::handleMediaPathChanged(const QString &path)
{
    if (m_subtitleTrackMediaPath.isEmpty() ||
        path == m_subtitleTrackMediaPath)
    {
        return;
    }

    setSubtitleTrackId(-1);
    m_subtitleCues.clear();
    stop();
}

void AsrController::handleFileLifecycleChange()
{
    if (m_subtitleTrackId < 0 && !m_active && !m_running)
    {
        return;
    }

    setSubtitleTrackId(-1);
    m_subtitleCues.clear();
    stop();
}

void AsrController::handleSidChanged(int64_t sid)
{
    if (m_subtitleTrackId >= 0 && sid != m_subtitleTrackId)
    {
        m_previousSubtitleSid = sid;
        updateCurrentText();
    }
}

void AsrController::addSubtitle(
    int generation,
    int segmentGeneration,
    const SubtitleEntry &subtitle)
{
    if (generation != m_generation ||
        segmentGeneration != m_segmentGeneration ||
        !m_active ||
        m_controller == nullptr ||
        m_subtitleTrackId < 0)
    {
        return;
    }

    const QString text = subtitle.text.trimmed();
    if (text.isEmpty() ||
        !std::isfinite(subtitle.start) ||
        !std::isfinite(subtitle.end) ||
        subtitle.end <= subtitle.start + SUBTITLE_TIME_DELTA)
    {
        return;
    }

    MpvPlayer *player = m_controller->player();
    MpvState *state = player == nullptr ? nullptr : player->state();
    if (state == nullptr || !player->isFileLoaded() ||
        state->path() != m_subtitleTrackMediaPath)
    {
        return;
    }

    auto cue = m_subtitleCues.begin();
    while (cue != m_subtitleCues.end())
    {
        const bool overlaps =
            cue->subtitle.start < subtitle.end - SUBTITLE_TIME_DELTA &&
            subtitle.start + SUBTITLE_TIME_DELTA < cue->subtitle.end;
        if (!overlaps)
        {
            ++cue;
            continue;
        }

        if (!m_controller->removeSubtitle(m_subtitleTrackId, cue->id))
        {
            return;
        }
        cue = m_subtitleCues.erase(cue);
    }

    const qint64 cueId = m_controller->addSubtitle(
        m_subtitleTrackId,
        subtitle.start,
        subtitle.end,
        text
    );
    if (cueId < 0)
    {
        return;
    }

    MemorySubtitleCue newCue{
        .id = cueId,
        .subtitle = SubtitleEntry{
            .text = text,
            .start = subtitle.start,
            .end = subtitle.end,
        },
    };
    const auto insertion = std::lower_bound(
        m_subtitleCues.begin(),
        m_subtitleCues.end(),
        newCue.subtitle.start,
        [] (const MemorySubtitleCue &item, double start)
        {
            return item.subtitle.start < start;
        }
    );
    m_subtitleCues.insert(insertion, std::move(newCue));
    updateCurrentText();
}

void AsrController::updateCurrentText()
{
    if (!m_active || m_controller == nullptr || m_subtitleTrackId < 0)
    {
        setCurrentText({});
        return;
    }

    MpvPlayer *player = m_controller->player();
    MpvState *state = player == nullptr ? nullptr : player->state();
    if (state == nullptr || state->sid() != m_subtitleTrackId ||
        state->subtitle() == nullptr)
    {
        setCurrentText({});
        return;
    }

    setCurrentText(state->subtitle()->text());
}

void AsrController::handlePositionChanged(double position)
{
    updateCurrentText();
    MpvPlayer *player = m_controller == nullptr ? nullptr :
        m_controller->player();
    MpvState *state = player == nullptr ? nullptr : player->state();
    if (state == nullptr)
    {
        return;
    }

    double elapsedSeconds = 0.0;
    if (m_lastPositionTimer.isValid())
    {
        elapsedSeconds =
            static_cast<double>(m_lastPositionTimer.elapsed()) / 1000.0;
    }
    m_lastPositionTimer.restart();

    if (m_retryPaused)
    {
        m_lastPosition = position;
        if (m_retryAwaitingSeekPosition)
        {
            m_retryPaused = false;
            m_retryAwaitingSeekPosition = false;
            requestRestart(position);
            if (!m_running && m_active && m_controller != nullptr)
            {
                selectAsync(m_controller);
            }
        }
        return;
    }

    bool seeked = false;
    if (m_lastPosition >= 0.0)
    {
        const double positionDelta = position - m_lastPosition;
        const double absoluteDelta = std::abs(positionDelta);
        if (absoluteDelta > SEEK_RESTART_THRESHOLD_SECONDS)
        {
            seeked = positionDelta < 0.0 ||
                state->pause() ||
                positionDelta >
                    elapsedSeconds + SEEK_RESTART_CLOCK_SLOP_SECONDS;
        }
    }
    m_lastPosition = position;

    if (seeked)
    {
        requestRestart(position);
        if (!m_running && m_active && m_controller != nullptr)
        {
            selectAsync(m_controller);
        }
        return;
    }

    if (m_running || m_retryPaused)
    {
        return;
    }

    if (position >= state->duration() - MIN_WORK_WINDOW_SECONDS)
    {
        return;
    }

    const double uncovered = skipCoveredPosition(position);
    if (uncovered <= position + SUBTITLE_TIME_DELTA)
    {
        selectAsync(m_controller);
    }
}

void AsrController::handleSeeked()
{
    if (m_retryPaused)
    {
        m_retryAwaitingSeekPosition = true;
    }
}

void AsrController::requestRestart(double position)
{
    ++m_segmentGeneration;
    m_restartPosition = position;
    m_restartRequested = true;
    if (m_abort)
    {
        m_abort->store(true);
    }
}

void AsrController::requestReconfigure()
{
    Settings *settings = m_context != nullptr ? m_context->settings() : nullptr;
    if (settings == nullptr || !settings->asrEnabled())
    {
        stop();
        return;
    }

    if (!m_active)
    {
        return;
    }

    m_retryPaused = false;
    m_retryAwaitingSeekPosition = false;
    ++m_segmentGeneration;
    m_reconfigureRequested = true;
    if (m_abort)
    {
        m_abort->store(true);
    }
    if (!m_running)
    {
        m_backend.reset();
        QMetaObject::invokeMethod(
            this,
            &AsrController::restartAfterReconfigure,
            Qt::QueuedConnection
        );
    }
}

void AsrController::restartAfterReconfigure()
{
    Settings *settings = m_context != nullptr ? m_context->settings() : nullptr;
    if (!m_reconfigureRequested || m_running || m_selectInFlight ||
        !m_active || m_controller == nullptr || settings == nullptr ||
        !settings->asrEnabled())
    {
        return;
    }

    /* Consume before selection, including attempts that fail validation. */
    m_reconfigureRequested = false;
    m_isReconfiguring = true;
    m_retryPaused = false;
    m_retryAwaitingSeekPosition = false;
    m_backend.reset();
    selectAsync(m_controller);
}

bool AsrController::consumeRestart(double &position)
{
    const bool requested = m_restartRequested.exchange(false);
    if (requested)
    {
        position = m_restartPosition;
    }
    return requested;
}

double AsrController::skipCoveredPosition(double position) const
{
    bool advanced = true;
    while (advanced)
    {
        advanced = false;
        for (const MemorySubtitleCue &cue : m_subtitleCues)
        {
            const SubtitleEntry &subtitle = cue.subtitle;
            if (subtitle.start <= position + SUBTITLE_TIME_DELTA &&
                position < subtitle.end)
            {
                position = subtitle.end;
                advanced = true;
                break;
            }
        }
    }
    return position;
}

double AsrController::nextSubtitleStart(double position) const
{
    double next = std::numeric_limits<double>::infinity();
    for (const MemorySubtitleCue &cue : m_subtitleCues)
    {
        const SubtitleEntry &subtitle = cue.subtitle;
        if (subtitle.start > position + SUBTITLE_TIME_DELTA &&
            subtitle.start < next)
        {
            next = subtitle.start;
        }
    }
    return next;
}

QString AsrController::selectedModelPath() const
{
    if (m_context == nullptr || m_context->settings() == nullptr)
    {
        return {};
    }

    Settings *settings = m_context->settings();
    if (settings->asrBackend() == Keys::Asr::BACKEND_WHISPER)
    {
        return m_whisperController->selectedModelPath();
    }
    return {};
}
