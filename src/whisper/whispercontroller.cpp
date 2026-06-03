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

#include "whisper/whispercontroller.h"

#include <cmath>
#include <limits>
#include <utility>

#include <QDir>
#include <QElapsedTimer>
#include <QFile>
#include <QFileInfo>
#include <QMetaObject>
#include <QNetworkReply>
#include <QNetworkRequest>
#include <QPointer>
#include <QThread>
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
#include "setting/settings.h"
#include "state/context.h"
#include "subtitle/subtitlelistmodel.h"
#include "util/directoryutils.h"

#ifdef MEMENTO_WHISPER_SUPPORT
#include "whisper/whispermodel.h"
#endif // MEMENTO_WHISPER_SUPPORT

static constexpr const char *KEY_ERROR = "error";
static constexpr const char *MODEL_CUSTOM = "custom";
static constexpr const char *MODEL_PREFIX = "ggml-";
static constexpr const char *MODEL_SUFFIX = ".bin";
static constexpr const char *WHISPER_MODEL_URL =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";
static constexpr const char *WHISPER_VAD_URL =
    "https://huggingface.co/ggml-org/whisper-vad/resolve/main/";
static constexpr const char *WHISPER_MODEL_DIR = "models";
static constexpr const char *VAD_SILERO_5 = "ggml-silero-v5.1.2.bin";
static constexpr const char *VAD_SILERO_6 = "ggml-silero-v6.2.0.bin";
static constexpr double WORK_WINDOW_SECONDS = 30.0;
static constexpr double MIN_WORK_WINDOW_SECONDS = 0.25;
static constexpr double SEEK_RESTART_THRESHOLD_SECONDS = 5.0;
static constexpr double SUBTITLE_TIME_DELTA = 0.0001;
static constexpr int WHISPER_GPU_DEVICE = 0;
static constexpr int MAX_TRANSCRIPTION_FAILURES = 3;

WhisperController::WhisperController(Context *context, QObject *parent) :
    QObject(parent),
    m_context(context)
{
    if (m_context != nullptr)
    {
        m_subtitles = new SubtitleListModel(m_context, this);

        if (Settings *settings = m_context->settings())
        {
            connect(
                settings,
                &Settings::whisperEnabledChanged,
                this,
                [this] { requestReconfigure(); }
            );
            connect(
                settings,
                &Settings::whisperModelChanged,
                this,
                [this] { requestReconfigure(); }
            );
            connect(
                settings,
                &Settings::whisperCustomModelChanged,
                this,
                [this] { requestReconfigure(); }
            );
            connect(
                settings,
                &Settings::whisperVadEnabledChanged,
                this,
                [this] { requestReconfigure(); }
            );
            connect(
                settings,
                &Settings::whisperVadModelChanged,
                this,
                [this] { requestReconfigure(); }
            );
            connect(
                settings,
                &Settings::whisperUseGpuChanged,
                this,
                [this] { requestReconfigure(); }
            );
            connect(
                settings,
                &Settings::whisperThreadsChanged,
                this,
                [this] { requestReconfigure(); }
            );
            connect(
                settings,
                &Settings::whisperBestOfChanged,
                this,
                [this] { requestReconfigure(); }
            );
            connect(
                settings,
                &Settings::whisperBeamSizeChanged,
                this,
                [this] { requestReconfigure(); }
            );
            connect(
                settings,
                &Settings::whisperFlashAttentionChanged,
                this,
                [this] { requestReconfigure(); }
            );
        }
    }
}

WhisperController::~WhisperController()
{
    stop();
}

bool WhisperController::active() const noexcept
{
    return m_active;
}

bool WhisperController::running() const noexcept
{
    return m_running;
}

QString WhisperController::currentText() const
{
    return m_currentText;
}

bool WhisperController::downloadRunning() const noexcept
{
    return m_downloadRunning;
}

qreal WhisperController::downloadProgress() const noexcept
{
    if (m_downloadTotal <= 0)
    {
        return m_downloadRunning ? 0.0 : 1.0;
    }
    return qBound(0.0, static_cast<qreal>(m_downloadReceived) /
        static_cast<qreal>(m_downloadTotal), 1.0);
}

qint64 WhisperController::downloadReceived() const noexcept
{
    return m_downloadReceived;
}

qint64 WhisperController::downloadTotal() const noexcept
{
    return m_downloadTotal;
}

qint64 WhisperController::downloadSpeed() const noexcept
{
    return m_downloadSpeed;
}

QString WhisperController::downloadName() const
{
    return m_downloadName;
}

QCoro::QmlTask WhisperController::select(MpvController *controller)
{
    return selectAsync(controller);
}

void WhisperController::stop()
{
    ++m_generation;
    ++m_segmentGeneration;
    m_restartRequested = false;
    m_reconfigureRequested = false;

#ifdef MEMENTO_WHISPER_SUPPORT
    if (m_abort)
    {
        m_abort->store(true);
    }
#endif // MEMENTO_WHISPER_SUPPORT

    if (m_context != nullptr &&
        m_context->subtitleLists() != nullptr &&
        m_context->subtitleLists()->primary() == m_subtitles)
    {
        m_context->subtitleLists()->setPrimary(nullptr);
    }
    setActive(false);
    setCurrentText({});
    m_controller = nullptr;
}

QString WhisperController::modelsDirectory() const
{
    return QDir(DirectoryUtils::getConfigDir()).filePath(WHISPER_MODEL_DIR);
}

bool WhisperController::hasAnyModel() const
{
    QDir dir(modelsDirectory());
    const QFileInfoList entries = dir.entryInfoList(
        QStringList{"*.bin"},
        QDir::Files | QDir::Readable
    );
    for (const QFileInfo &entry : entries)
    {
        const QString filename = entry.fileName();
        if (filename != VAD_SILERO_5 && filename != VAD_SILERO_6)
        {
            return true;
        }
    }
    return false;
}

bool WhisperController::modelAvailable(const QString &model) const
{
    if (!isManagedModel(model))
    {
        return false;
    }
    return QFileInfo::exists(resolveModelPath(modelFilename(model)));
}

bool WhisperController::selectedModelAvailable() const
{
    const QString path = selectedModelPath();
    return !path.isEmpty() && QFileInfo::exists(path);
}

bool WhisperController::selectedModelDownloadable() const
{
    return m_context != nullptr &&
        m_context->settings() != nullptr &&
        isManagedModel(m_context->settings()->whisperModel());
}

QCoro::QmlTask WhisperController::downloadModel(const QString &model)
{
    return downloadModelAsync(model);
}

QCoro::Task<QVariantMap> WhisperController::downloadModelAsync(QString model)
{
    QVariantMap result;

#ifndef MEMENTO_WHISPER_SUPPORT
    Q_UNUSED(model)
    result[KEY_ERROR] = tr(
        "Whisper subtitle support is not available in this build.");
    co_return result;
#else
    if (m_downloadRunning)
    {
        result[KEY_ERROR] = tr("A Whisper model is already downloading.");
        co_return result;
    }

    if (!isManagedModel(model))
    {
        result[KEY_ERROR] = tr("This Whisper model cannot be downloaded.");
        co_return result;
    }

    const QString path = resolveModelPath(modelFilename(model));
    if (QFileInfo::exists(path))
    {
        const qint64 size = QFileInfo(path).size();
        setDownloadName(model);
        setDownloadProgress(size, size, 0);
        result["path"] = path;
        if (m_context != nullptr &&
            m_context->settings() != nullptr &&
            m_context->settings()->whisperModel() == model)
        {
            requestReconfigure();
        }
        co_return result;
    }

    setDownloadName(model);
    setDownloadProgress(0, 0, 0);
    setDownloadRunning(true);
    const bool ok = co_await ensureDownloaded(
        whisperModelUrl(model),
        path,
        model,
        true
    );
    setDownloadRunning(false);

    if (!ok)
    {
        result[KEY_ERROR] = tr("Could not download Whisper model: %1")
            .arg(model);
        co_return result;
    }

    result["path"] = path;
    if (m_context != nullptr &&
        m_context->settings() != nullptr &&
        m_context->settings()->whisperModel() == model)
    {
        requestReconfigure();
    }
    co_return result;
#endif // MEMENTO_WHISPER_SUPPORT
}

QCoro::Task<QVariantMap> WhisperController::selectAsync(
    MpvController *controller)
{
    QVariantMap result;

#ifndef MEMENTO_WHISPER_SUPPORT
    Q_UNUSED(controller)
    result[KEY_ERROR] = tr(
        "Whisper subtitle support is not available in this build.");
    co_return result;
#else
    if (m_running)
    {
        result[KEY_ERROR] = tr("Whisper is already transcribing.");
        co_return result;
    }

    if (m_context == nullptr ||
        m_context->settings() == nullptr ||
        !m_context->settings()->whisperEnabled())
    {
        result[KEY_ERROR] = tr("Whisper subtitles are disabled.");
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
    if (state->path().isEmpty() || state->duration() <= 0.0)
    {
        result[KEY_ERROR] = tr("No media is loaded.");
        co_return result;
    }

    Settings *settings = m_context->settings();
    setRunning(true);
    const QString modelPath = selectedModelPath();
    if (modelPath.isEmpty() || !QFileInfo::exists(modelPath))
    {
        setRunning(false);
        result["missingModel"] = true;
        result["path"] = modelPath;
        result[KEY_ERROR] = tr("Whisper model was not found: %1")
            .arg(modelPath);
        co_return result;
    }

    QString vadModelPath;
    if (settings->whisperVadEnabled())
    {
        vadModelPath = resolveModelPath(settings->whisperVadModel());
        const QUrl vadUrl = vadModelUrl(settings->whisperVadModel());
        if (!vadUrl.isEmpty() && !QFileInfo::exists(vadModelPath))
        {
            if (!co_await ensureDownloaded(vadUrl, vadModelPath))
            {
                setRunning(false);
                result[KEY_ERROR] = tr(
                    "Could not download Whisper VAD model: %1"
                ).arg(vadModelPath);
                co_return result;
            }
        }
        if (vadModelPath.isEmpty() || !QFileInfo::exists(vadModelPath))
        {
            setRunning(false);
            result[KEY_ERROR] = tr("Whisper VAD model was not found: %1")
                .arg(vadModelPath);
            co_return result;
        }
    }

    if (m_subtitles == nullptr)
    {
        setRunning(false);
        result[KEY_ERROR] = tr("No subtitle list is available.");
        co_return result;
    }

    if (m_context->subtitleLists() == nullptr)
    {
        setRunning(false);
        result[KEY_ERROR] = tr("No subtitle list manager is available.");
        co_return result;
    }

    const QString mediaPath = state->path();
    const bool useGpu = settings->whisperUseGpu();
    const int gpuDevice = WHISPER_GPU_DEVICE;
    const bool flashAttention = settings->whisperFlashAttention();
    const QString cacheSettingsKey =
        modelPath + "\n" +
        vadModelPath + "\n" +
        QString::number(useGpu) + "\n" +
        QString::number(settings->whisperThreads()) + "\n" +
        QString::number(settings->whisperBestOf()) + "\n" +
        QString::number(settings->whisperBeamSize()) + "\n" +
        QString::number(settings->whisperVadEnabled()) + "\n" +
        QString::number(flashAttention);
    if (m_cacheMediaPath != mediaPath ||
        m_cacheModelPath != modelPath ||
        m_cacheSettingsKey != cacheSettingsKey)
    {
        m_subtitles->clear();
        m_cacheMediaPath = mediaPath;
        m_cacheModelPath = modelPath;
        m_cacheSettingsKey = cacheSettingsKey;
    }

    const int generation = ++m_generation;
    ++m_segmentGeneration;
    m_restartRequested = false;
    m_reconfigureRequested = false;
    m_controller = controller;
    m_lastPosition = state->timePosition();

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
    m_context->subtitleLists()->setPrimary(m_subtitles);

    const int initialRows = m_subtitles->rowCount();
    double start = skipCoveredPosition(
        qBound(0.0, state->timePosition(), state->duration())
    );
    bool failed = false;
    bool fatalFailure = true;
    bool runtimeUseGpu = useGpu;
    bool runtimeFlashAttention = flashAttention;
    int consecutiveFailures = 0;

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
            result[KEY_ERROR] = tr("Media changed while Whisper was running.");
            break;
        }

        start = qBound(0.0, skipCoveredPosition(start), duration);
        if (start >= duration - MIN_WORK_WINDOW_SECONDS)
        {
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
        const int segmentGeneration = m_segmentGeneration;

        MpvAudioClipArgs args;
        args.start = start;
        args.end = end;
        args.normalize = false;
        args.extension = ".wav";

        const QString audioPath = m_controller->tempAudioClip(args);
        if (audioPath.isEmpty())
        {
            failed = true;
            result[KEY_ERROR] = tr("Could not extract audio for Whisper.");
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
            continue;
        }

        WhisperModel::Options options{
            .threads = settings->whisperThreads(),
            .bestOf = settings->whisperBestOf(),
            .beamSize = settings->whisperBeamSize(),
            .useVad = settings->whisperVadEnabled(),
            .vadModel = vadModelPath,
            .abort = m_abort,
            .segmentCallback = {},
        };

        QPointer<WhisperController> self(this);
        options.segmentCallback =
            [self, generation, segmentGeneration, start, end]
            (const SubtitleEntry &subtitle)
            {
                if (self == nullptr)
                {
                    return;
                }

                SubtitleEntry adjusted = subtitle;
                adjusted.start = qBound(start, adjusted.start + start, end);
                adjusted.end = qBound(start, adjusted.end + start, end);
                const Qt::ConnectionType connectionType =
                    QThread::currentThread() == self->thread() ?
                        Qt::DirectConnection :
                        Qt::BlockingQueuedConnection;

                QMetaObject::invokeMethod(
                    self,
                    [self, generation, segmentGeneration, adjusted]
                    {
                        if (self != nullptr)
                        {
                            self->addSubtitle(
                                generation,
                                segmentGeneration,
                                adjusted
                            );
                        }
                    },
                    connectionType
                );
            };

        auto canRetry = [this, generation]
        {
            return generation == m_generation &&
                m_active &&
                !m_reconfigureRequested &&
                !m_restartRequested;
        };

        WhisperModel::TranscriptionResult transcribeResult =
            co_await qCoro(
                model(
                    modelPath,
                    runtimeUseGpu,
                    gpuDevice,
                    runtimeFlashAttention
                )->transcribe(
                    audioPath,
                    options
                )
            ).takeResult();

        if (transcribeResult ==
                WhisperModel::TranscriptionResult::Failed &&
            canRetry() &&
            runtimeFlashAttention)
        {
            qWarning(
                "Whisper transcription failed with flash attention; "
                "retrying without flash attention."
            );
            runtimeFlashAttention = false;
            transcribeResult = co_await qCoro(
                model(
                    modelPath,
                    runtimeUseGpu,
                    gpuDevice,
                    runtimeFlashAttention
                )->transcribe(
                    audioPath,
                    options
                )
            ).takeResult();
        }

        if (transcribeResult ==
                WhisperModel::TranscriptionResult::Failed &&
            canRetry() &&
            runtimeUseGpu)
        {
            qWarning(
                "Whisper GPU transcription failed; retrying on CPU/BLAS."
            );
            runtimeUseGpu = false;
            runtimeFlashAttention = false;
            transcribeResult = co_await qCoro(
                model(
                    modelPath,
                    runtimeUseGpu,
                    gpuDevice,
                    runtimeFlashAttention
                )->transcribe(
                    audioPath,
                    options
                )
            ).takeResult();
        }

        QFile::remove(audioPath);

        if (m_reconfigureRequested)
        {
            break;
        }
        if (consumeRestart(restartPosition))
        {
            start = restartPosition;
            continue;
        }
        if (generation != m_generation || !m_active)
        {
            m_abort.reset();
            setRunning(false);
            co_return result;
        }
        if (transcribeResult == WhisperModel::TranscriptionResult::Canceled)
        {
            start = qBound(0.0, state->timePosition(), duration);
            continue;
        }
        if (transcribeResult != WhisperModel::TranscriptionResult::Success)
        {
            ++consecutiveFailures;
            if (consecutiveFailures >= MAX_TRANSCRIPTION_FAILURES)
            {
                failed = true;
                fatalFailure = false;
                result["fatal"] = false;
                result[KEY_ERROR] = tr(
                    "Whisper transcription paused after repeated decode "
                    "failures. Seek or change Whisper settings to retry."
                );
                break;
            }

            qWarning(
                "Whisper transcription failed; retrying from the current "
                "playback position."
            );
            start = qBound(0.0, state->timePosition(), duration);
            continue;
        }

        consecutiveFailures = 0;
        start = end;
    }

    const bool reconfigure = m_reconfigureRequested;
    m_reconfigureRequested = false;
    m_abort.reset();
    setRunning(false);
    updateCurrentText();

    if (generation != m_generation || !m_active)
    {
        co_return result;
    }
    if (reconfigure)
    {
        if (m_controller != nullptr)
        {
            co_return co_await selectAsync(m_controller);
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
    if (initialRows == 0 && m_subtitles->rowCount() == 0)
    {
        stop();
        result[KEY_ERROR] = tr("No speech was detected.");
        co_return result;
    }

    co_return result;
#endif // MEMENTO_WHISPER_SUPPORT
}

void WhisperController::setActive(bool value)
{
    if (m_active == value)
    {
        return;
    }
    m_active = value;
    emit activeChanged(m_active);
}

void WhisperController::setRunning(bool value)
{
    if (m_running == value)
    {
        return;
    }
    m_running = value;
    emit runningChanged(m_running);
}

void WhisperController::setCurrentText(QString value)
{
    if (m_currentText == value)
    {
        return;
    }
    m_currentText = std::move(value);
    emit currentTextChanged(m_currentText);
}

void WhisperController::setDownloadRunning(bool value)
{
    if (m_downloadRunning == value)
    {
        return;
    }
    m_downloadRunning = value;
    emit downloadRunningChanged(m_downloadRunning);
}

void WhisperController::setDownloadName(QString value)
{
    if (m_downloadName == value)
    {
        return;
    }
    m_downloadName = std::move(value);
    emit downloadNameChanged(m_downloadName);
}

void WhisperController::setDownloadProgress(
    qint64 received, qint64 total, qint64 speed)
{
    received = qMax<qint64>(0, received);
    total = qMax<qint64>(0, total);
    speed = qMax<qint64>(0, speed);

    if (m_downloadReceived == received &&
        m_downloadTotal == total &&
        m_downloadSpeed == speed)
    {
        return;
    }

    m_downloadReceived = received;
    m_downloadTotal = total;
    m_downloadSpeed = speed;
    emit downloadProgressChanged();
}

void WhisperController::addSubtitle(
    int generation,
    int segmentGeneration,
    const SubtitleEntry &subtitle)
{
    if (generation != m_generation ||
        segmentGeneration != m_segmentGeneration ||
        m_subtitles == nullptr)
    {
        return;
    }

    const QString text = subtitle.text.trimmed();
    if (text.isEmpty() ||
        subtitle.end <= subtitle.start + SUBTITLE_TIME_DELTA)
    {
        return;
    }

    if (m_context != nullptr &&
        m_context->subtitleLists() != nullptr &&
        m_context->subtitleLists()->primary() != m_subtitles)
    {
        m_context->subtitleLists()->setPrimary(m_subtitles);
    }

    m_subtitles->removeOverlapping(subtitle.start, subtitle.end);
    m_subtitles->addSubtitle(text, subtitle.start, subtitle.end);
    if (m_context != nullptr &&
        m_context->player() != nullptr &&
        m_context->player()->state() != nullptr)
    {
        m_subtitles->selectPosition(
            m_context->player()->state()->timePosition()
        );
    }
    updateCurrentText();
}

void WhisperController::handlePositionChanged(double position)
{
    updateCurrentText();

    if (!m_active)
    {
        m_lastPosition = position;
        return;
    }

    const bool seeked = m_lastPosition >= 0.0 &&
        std::abs(position - m_lastPosition) > SEEK_RESTART_THRESHOLD_SECONDS;
    m_lastPosition = position;

    if (seeked)
    {
        requestRestart(position);
        return;
    }

    if (m_running ||
        m_controller == nullptr ||
        m_context == nullptr ||
        m_context->player() == nullptr ||
        m_context->player()->state() == nullptr)
    {
        return;
    }

    MpvState *state = m_context->player()->state();
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

void WhisperController::requestRestart(double position)
{
    if (!m_active)
    {
        return;
    }

    if (m_context != nullptr &&
        m_context->player() != nullptr &&
        m_context->player()->state() != nullptr)
    {
        position = qBound(
            0.0,
            position,
            m_context->player()->state()->duration()
        );
    }
    else
    {
        position = qMax(0.0, position);
    }

    m_restartPosition = position;
    m_restartRequested = true;
    ++m_segmentGeneration;

#ifdef MEMENTO_WHISPER_SUPPORT
    if (m_abort)
    {
        m_abort->store(true);
    }
#endif // MEMENTO_WHISPER_SUPPORT

    if (!m_running && m_controller != nullptr)
    {
        selectAsync(m_controller);
    }
}

void WhisperController::requestReconfigure()
{
    if (!m_active)
    {
        return;
    }

    if (m_context != nullptr &&
        m_context->settings() != nullptr &&
        !m_context->settings()->whisperEnabled())
    {
        stop();
        return;
    }

    m_reconfigureRequested = true;

    double position = 0.0;
    if (m_context != nullptr &&
        m_context->player() != nullptr &&
        m_context->player()->state() != nullptr)
    {
        position = m_context->player()->state()->timePosition();
    }
    requestRestart(position);
}

bool WhisperController::consumeRestart(double &position)
{
    if (!m_restartRequested)
    {
        return false;
    }

    position = m_restartPosition;
    m_restartRequested = false;
    return true;
}

double WhisperController::skipCoveredPosition(double position) const
{
    if (m_subtitles == nullptr)
    {
        return position;
    }

    bool advanced = true;
    while (advanced)
    {
        advanced = false;
        for (const SubtitleEntry &subtitle : m_subtitles->items())
        {
            if (subtitle.start <= position + SUBTITLE_TIME_DELTA &&
                position < subtitle.end - SUBTITLE_TIME_DELTA)
            {
                position = subtitle.end;
                advanced = true;
                break;
            }
        }
    }
    return position;
}

double WhisperController::nextSubtitleStart(double position) const
{
    if (m_subtitles == nullptr)
    {
        return std::numeric_limits<double>::infinity();
    }

    double next = std::numeric_limits<double>::infinity();
    for (const SubtitleEntry &subtitle : m_subtitles->items())
    {
        if (subtitle.start > position + SUBTITLE_TIME_DELTA &&
            subtitle.start < next)
        {
            next = subtitle.start;
        }
    }
    return next;
}

void WhisperController::updateCurrentText()
{
    if (!m_active || m_context == nullptr)
    {
        setCurrentText({});
        return;
    }

    if (m_subtitles != nullptr &&
        m_context->subtitleLists() != nullptr &&
        m_context->subtitleLists()->primary() != m_subtitles)
    {
        m_context->subtitleLists()->setPrimary(m_subtitles);
    }

    if (m_context->player() == nullptr ||
        m_context->player()->state() == nullptr ||
        m_subtitles == nullptr)
    {
        setCurrentText({});
        return;
    }

    const double position = m_context->player()->state()->timePosition();
    for (const SubtitleEntry &subtitle : m_subtitles->items())
    {
        if (subtitle.start <= position && position < subtitle.end)
        {
            setCurrentText(subtitle.text);
            return;
        }
    }
    setCurrentText({});
}

QString WhisperController::resolveModelPath(const QString &path) const
{
    if (path.isEmpty())
    {
        return {};
    }

    const QFileInfo info(path);
    if (info.isAbsolute())
    {
        return path;
    }

    const QDir modelDir(modelsDirectory());
    QStringList filenames{path};
    if (!path.endsWith(MODEL_SUFFIX))
    {
        filenames.append(path + MODEL_SUFFIX);
    }

    for (const QString &filename : filenames)
    {
        const QString candidate = modelDir.filePath(filename);
        if (QFileInfo::exists(candidate))
        {
            return candidate;
        }
    }

    const bool bareFilename = info.fileName() == path;
    if (bareFilename)
    {
        return modelDir.filePath(filenames.last());
    }

    return info.absoluteFilePath();
}

QString WhisperController::selectedModelPath() const
{
    if (m_context == nullptr || m_context->settings() == nullptr)
    {
        return {};
    }

    const QString model = m_context->settings()->whisperModel();
    if (model == MODEL_CUSTOM)
    {
        return resolveModelPath(m_context->settings()->whisperCustomModel());
    }

    return resolveModelPath(modelFilename(model));
}

QCoro::Task<bool> WhisperController::ensureDownloaded(
    const QUrl &url,
    const QString &path,
    const QString &name,
    bool reportProgress)
{
    if (!url.isValid() || path.isEmpty())
    {
        co_return false;
    }
    if (QFileInfo::exists(path))
    {
        if (reportProgress)
        {
            const qint64 size = QFileInfo(path).size();
            setDownloadName(name);
            setDownloadProgress(size, size, 0);
        }
        co_return true;
    }

    const QFileInfo info(path);
    if (!QDir().mkpath(info.absolutePath()))
    {
        qWarning("Could not create Whisper model directory.");
        co_return false;
    }

    const QString partialPath = path + ".download";
    QFile::remove(partialPath);
    QFile file(partialPath);
    if (!file.open(QFile::WriteOnly))
    {
        qWarning("Could not open Whisper model download target.");
        co_return false;
    }

    QNetworkRequest req{url};
    req.setAttribute(
        QNetworkRequest::RedirectPolicyAttribute,
        QNetworkRequest::UserVerifiedRedirectPolicy
    );
    std::unique_ptr<QNetworkReply> reply{m_manager.get(std::move(req))};
    connect(
        reply.get(), &QNetworkReply::redirected,
        reply.get(), &QNetworkReply::redirectAllowed
    );
    QElapsedTimer timer;
    timer.start();
    if (reportProgress)
    {
        connect(
            reply.get(),
            &QNetworkReply::downloadProgress,
            this,
            [this, &timer] (qint64 received, qint64 total)
            {
                const qint64 elapsed = qMax<qint64>(1, timer.elapsed());
                setDownloadProgress(
                    received,
                    total,
                    (received * 1000) / elapsed
                );
            }
        );
    }
    connect(
        reply.get(), &QNetworkReply::readyRead,
        reply.get(),
        [&file, reply = reply.get()]
        {
            file.write(reply->readAll());
        }
    );
    co_await reply.get();
    file.write(reply->readAll());
    file.close();

    const QVariant statusCode = reply->attribute(
        QNetworkRequest::HttpStatusCodeAttribute
    );
    if (reply->error() != QNetworkReply::NetworkError::NoError ||
        (statusCode.isValid() && statusCode.toInt() >= 400))
    {
        qWarning(
            "Whisper model download failed: %s",
            qUtf8Printable(reply->errorString())
        );
        QFile::remove(partialPath);
        co_return false;
    }

    QFile::remove(path);
    if (!QFile::rename(partialPath, path))
    {
        qWarning("Could not finalize Whisper model download.");
        QFile::remove(partialPath);
        co_return false;
    }

    if (reportProgress)
    {
        const qint64 size = QFileInfo(path).size();
        setDownloadProgress(size, size, 0);
    }
    co_return true;
}

QString WhisperController::modelFilename(const QString &model) const
{
    return QString("%1%2%3").arg(MODEL_PREFIX, model, MODEL_SUFFIX);
}

bool WhisperController::isManagedModel(const QString &model) const
{
    static const QStringList MODELS{
        "tiny",
        "base",
        "small",
        "medium",
        "large-v3",
        "large-v3-turbo",
    };
    return MODELS.contains(model);
}

QUrl WhisperController::whisperModelUrl(const QString &model) const
{
    return QUrl(QString("%1%2").arg(WHISPER_MODEL_URL, modelFilename(model)));
}

QUrl WhisperController::vadModelUrl(const QString &modelPath) const
{
    const QFileInfo info(modelPath);
    if (info.isAbsolute())
    {
        return {};
    }

    const QString filename = info.fileName();
    if (filename != VAD_SILERO_5 && filename != VAD_SILERO_6)
    {
        return {};
    }

    return QUrl(QString("%1%2").arg(WHISPER_VAD_URL, filename));
}

#ifdef MEMENTO_WHISPER_SUPPORT
WhisperModel *WhisperController::model(
    const QString &modelPath,
    bool useGpu,
    int gpuDevice,
    bool flashAttention)
{
    if (!m_model ||
        m_modelPath != modelPath ||
        m_useGpu != useGpu ||
        m_gpuDevice != gpuDevice ||
        m_flashAttention != flashAttention)
    {
        m_model = std::make_unique<WhisperModel>(
            modelPath,
            useGpu,
            gpuDevice,
            flashAttention
        );
        m_modelPath = modelPath;
        m_useGpu = useGpu;
        m_gpuDevice = gpuDevice;
        m_flashAttention = flashAttention;
    }
    return m_model.get();
}
#endif // MEMENTO_WHISPER_SUPPORT
