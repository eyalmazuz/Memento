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

#include <cmath>
#include <limits>
#include <utility>

#include <QDir>
#include <QElapsedTimer>
#include <QFile>
#include <QFileInfo>
#include <QMetaObject>
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
#include "subtitle/subtitlelistmodel.h"
#include "manager/downloadmanager.h"
#include "asr/whisper/whispercontroller.h"

static constexpr const char *KEY_ERROR = "error";
static constexpr const char *MODEL_CUSTOM = "custom";
static constexpr const char *MODEL_SUFFIX = ".bin";

static constexpr double WORK_WINDOW_SECONDS = 30.0;
static constexpr double MIN_WORK_WINDOW_SECONDS = 0.25;
static constexpr double SEEK_RESTART_THRESHOLD_SECONDS = 5.0;
static constexpr double SEEK_RESTART_CLOCK_SLOP_SECONDS = 2.0;
static constexpr double SUBTITLE_TIME_DELTA = 0.0001;
static constexpr int WHISPER_GPU_DEVICE = 0;
static constexpr int MAX_TRANSCRIPTION_FAILURES = 3;

AsrController::AsrController(Context *context, QObject *parent) :
    QObject(parent),
    m_context(context),
    m_downloader(context != nullptr ? context->downloadManager() : nullptr),
    m_whisperController(new WhisperController(context, this))
{
    if (m_context != nullptr)
    {
        m_subtitles = new SubtitleListModel(m_context, this);

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
            connect(
                settings,
                &Settings::whisperEnabledChanged,
                this,
                &AsrController::requestReconfigure
            );
            connect(
                settings,
                &Settings::whisperModelChanged,
                this,
                &AsrController::requestReconfigure
            );
            connect(
                settings,
                &Settings::whisperCustomModelChanged,
                this,
                &AsrController::requestReconfigure
            );
            connect(
                settings,
                &Settings::whisperVadEnabledChanged,
                this,
                &AsrController::requestReconfigure
            );
            connect(
                settings,
                &Settings::whisperVadModelChanged,
                this,
                &AsrController::requestReconfigure
            );
            connect(
                settings,
                &Settings::whisperUseGpuChanged,
                this,
                &AsrController::requestReconfigure
            );
            connect(
                settings,
                &Settings::whisperThreadsChanged,
                this,
                &AsrController::requestReconfigure
            );
            connect(
                settings,
                &Settings::whisperBestOfChanged,
                this,
                &AsrController::requestReconfigure
            );
            connect(
                settings,
                &Settings::whisperBeamSizeChanged,
                this,
                &AsrController::requestReconfigure
            );
            connect(
                settings,
                &Settings::whisperFlashAttentionChanged,
                this,
                &AsrController::requestReconfigure
            );

        }
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
    return selectAsync(controller);
}

void AsrController::stop()
{
    ++m_selectGeneration;
    ++m_generation;
    ++m_segmentGeneration;
    m_restartRequested = false;
    m_reconfigureRequested = false;

    if (m_abort)
    {
        m_abort->store(true);
    }

    if (m_context != nullptr &&
        m_context->subtitleLists() != nullptr &&
        m_context->subtitleLists()->primary() == m_subtitles &&
        m_context->subtitleLists()->primarySource() == SubtitleLists::Internal)
    {
        m_context->subtitleLists()->setPrimary(nullptr, SubtitleLists::None);
    }
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
    QDir dir(modelsDirectory());
    const QFileInfoList entries = dir.entryInfoList(
        QStringList{"*"},
        QDir::Files | QDir::Readable
    );
    for (const QFileInfo &entry : entries)
    {
        const QString filename = entry.fileName();
        if (!filename.endsWith(MODEL_SUFFIX) ||
            filename == "ggml-silero-v5.1.2.bin" ||
            filename == "ggml-silero-v6.2.0.bin")
        {
            continue;
        }
        return true;
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

#ifndef MEMENTO_ASR_BACKEND_SUPPORT
    Q_UNUSED(controller)
    result[KEY_ERROR] = tr(
        "ASR subtitle support is not available in this build.");
    co_return result;
#else
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
    if (state->path().isEmpty() || state->duration() <= 0.0)
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

    const bool whisperBackend = backendName == Keys::Asr::BACKEND_WHISPER;

    QString vadModelPath;
    if (whisperBackend && settings->whisperVadEnabled())
    {
        vadModelPath = m_whisperController->resolveModelPath(
            settings->whisperVadModel(),
            QStringList{MODEL_SUFFIX}
        );
        const QUrl vadUrl = m_whisperController->vadModelUrl(settings->whisperVadModel());
        if (!vadUrl.isEmpty() && !QFileInfo::exists(vadModelPath))
        {
            if (!co_await m_downloader->download(vadUrl, vadModelPath, settings->whisperVadModel()))
            {
                setRunning(false);
                result[KEY_ERROR] = tr(
                    "Could not download Whisper VAD model: %1"
                ).arg(vadModelPath);
                co_return result;
            }
        }
        if (selectGeneration != m_selectGeneration)
        {
            co_return result;
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
    const bool useGpu = whisperBackend && settings->whisperUseGpu();
    const int gpuDevice = WHISPER_GPU_DEVICE;
    const bool flashAttention =
        whisperBackend && settings->whisperFlashAttention();
    const QString cacheSettingsKey =
        backendName + "\n" +
        modelPath + "\n" +
        vadModelPath + "\n" +
        QString::number(useGpu) + "\n" +
        QString::number(settings->whisperThreads()) + "\n" +
        QString::number(settings->whisperBestOf()) + "\n" +
        QString::number(settings->whisperBeamSize()) + "\n" +
        QString::number(settings->whisperVadEnabled()) + "\n" +
        QString::number(flashAttention) + "\n";
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
    m_context->subtitleLists()->setPrimary(
        m_subtitles,
        SubtitleLists::Internal
    );

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
            result[KEY_ERROR] = tr("Media changed while ASR was running.");
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
        args.preset = MpvAudioClipArgs::WhisperPcm16Wav;

        const QString audioPath = m_controller->tempAudioClip(args);
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
            continue;
        }

        AsrTranscriptionOptions options{
            .threads = settings->whisperThreads(),
            .bestOf = settings->whisperBestOf(),
            .beamSize = settings->whisperBeamSize(),
            .useVad = settings->whisperVadEnabled(),
            .vadModel = vadModelPath,
            .language = QStringLiteral("ja"),
            .abort = m_abort,
            .segmentCallback = {},
        };

        QPointer<AsrController> self(this);
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
                    Qt::QueuedConnection
                );
            };

        auto canRetry = [this, generation]
        {
            return generation == m_generation &&
                m_active &&
                !m_reconfigureRequested &&
                !m_restartRequested;
        };

        AsrBackend *currentBackend = backend(
            backendName,
            modelPath,
            runtimeUseGpu,
            gpuDevice,
            runtimeFlashAttention
        );

        if (currentBackend == nullptr)
        {
            failed = true;
            result[KEY_ERROR] = tr("Could not instantiate ASR backend.");
            QFile::remove(audioPath);
            break;
        }

        AsrTranscriptionResult transcribeResult =
            co_await qCoro(
                currentBackend->transcribe(
                    audioPath,
                    options
                )
            ).takeResult();

        if (transcribeResult ==
                AsrTranscriptionResult::Failed &&
            canRetry() &&
            whisperBackend &&
            runtimeFlashAttention)
        {
            qWarning(
                "Whisper transcription failed with flash attention; "
                "retrying without flash attention."
            );
            runtimeFlashAttention = false;
            currentBackend = backend(
                backendName,
                modelPath,
                runtimeUseGpu,
                gpuDevice,
                runtimeFlashAttention
            );
            if (currentBackend != nullptr)
            {
                transcribeResult = co_await qCoro(
                    currentBackend->transcribe(
                        audioPath,
                        options
                    )
                ).takeResult();
            }
        }

        if (transcribeResult ==
                AsrTranscriptionResult::Failed &&
            canRetry() &&
            whisperBackend &&
            runtimeUseGpu)
        {
            qWarning(
                "Whisper GPU transcription failed; retrying on CPU/BLAS."
            );
            runtimeUseGpu = false;
            runtimeFlashAttention = false;
            currentBackend = backend(
                backendName,
                modelPath,
                runtimeUseGpu,
                gpuDevice,
                runtimeFlashAttention
            );
            if (currentBackend != nullptr)
            {
                transcribeResult = co_await qCoro(
                    currentBackend->transcribe(
                        audioPath,
                        options
                    )
                ).takeResult();
            }
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
        if (transcribeResult == AsrTranscriptionResult::Canceled)
        {
            start = qBound(0.0, state->timePosition(), duration);
            continue;
        }
        if (transcribeResult != AsrTranscriptionResult::Success)
        {
            ++consecutiveFailures;
            if (consecutiveFailures >= MAX_TRANSCRIPTION_FAILURES)
            {
                failed = true;
                fatalFailure = false;
                result["fatal"] = false;
                result[KEY_ERROR] = tr(
                    "ASR transcription paused after repeated decode "
                    "failures. Seek or change ASR settings to retry."
                );
                break;
            }

            qWarning(
                "ASR transcription failed; retrying from the current "
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

void AsrController::addSubtitle(
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
        (m_context->subtitleLists()->primary() != m_subtitles ||
         m_context->subtitleLists()->primarySource() !=
            SubtitleLists::Internal))
    {
        m_context->subtitleLists()->setPrimary(
            m_subtitles,
            SubtitleLists::Internal
        );
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

void AsrController::updateCurrentText()
{
    if (!m_active || m_context == nullptr)
    {
        setCurrentText({});
        return;
    }

    if (m_subtitles != nullptr &&
        m_context->subtitleLists() != nullptr &&
        (m_context->subtitleLists()->primary() != m_subtitles ||
         m_context->subtitleLists()->primarySource() !=
            SubtitleLists::Internal))
    {
        m_context->subtitleLists()->setPrimary(
            m_subtitles,
            SubtitleLists::Internal
        );
    }

    if (m_context->player() == nullptr ||
        m_context->player()->state() == nullptr ||
        m_subtitles == nullptr)
    {
        setCurrentText({});
        return;
    }

    const double position = m_context->player()->state()->timePosition();
    const std::vector<SubtitleEntry> &subtitles = m_subtitles->items();
    bool found = false;
    for (size_t i = 0; i < subtitles.size(); ++i)
    {
        const SubtitleEntry &subtitle = subtitles[i];
        if (subtitle.start <= position && position < subtitle.end)
        {
            QItemSelectionModel *selection = m_subtitles->selectionModel();
            if (selection == nullptr ||
                !selection->isRowSelected(
                    static_cast<int>(i),
                    QModelIndex()
                ))
            {
                m_subtitles->selectPosition(position);
            }
            m_context->subtitleLists()->setPrimarySubtitle(
                subtitle.text,
                subtitle.start,
                subtitle.end
            );
            setCurrentText(subtitle.text);
            found = true;
            break;
        }
    }

    if (!found)
    {
        if (QItemSelectionModel *selection = m_subtitles->selectionModel())
        {
            if (selection->hasSelection())
            {
                selection->clear();
            }
        }
        m_context->subtitleLists()->clearPrimarySubtitle();
        setCurrentText({});
    }
}

void AsrController::handlePositionChanged(double position)
{
    updateCurrentText();

    double elapsedSeconds = 0.0;
    if (m_lastPositionTimer.isValid())
    {
        elapsedSeconds =
            static_cast<double>(m_lastPositionTimer.elapsed()) / 1000.0;
    }
    m_lastPositionTimer.restart();

    bool seeked = false;
    if (m_lastPosition >= 0.0)
    {
        const double positionDelta = position - m_lastPosition;
        const double absoluteDelta = std::abs(positionDelta);
        if (absoluteDelta > SEEK_RESTART_THRESHOLD_SECONDS)
        {
            MpvState *state = nullptr;
            if (m_context != nullptr &&
                m_context->player() != nullptr)
            {
                state = m_context->player()->state();
            }

            seeked = positionDelta < 0.0 ||
                state == nullptr ||
                state->pause() ||
                positionDelta >
                    elapsedSeconds + SEEK_RESTART_CLOCK_SLOP_SECONDS;
        }
    }
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

void AsrController::requestRestart(double position)
{
    m_restartPosition = position;
    m_restartRequested = true;
    if (m_abort)
    {
        m_abort->store(true);
    }
}

void AsrController::requestReconfigure()
{
    m_reconfigureRequested = true;
    if (m_abort)
    {
        m_abort->store(true);
    }
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

AsrBackend *AsrController::backend(
    const QString &backendName,
    const QString &modelPath,
    bool useGpu,
    int gpuDevice,
    bool flashAttention)
{
    if (!m_backend ||
        m_backendName != backendName ||
        m_modelPath != modelPath ||
        m_useGpu != useGpu ||
        m_gpuDevice != gpuDevice ||
        m_flashAttention != flashAttention)
    {
        m_backend.reset();
        if (backendName == Keys::Asr::BACKEND_WHISPER)
        {
            m_backend = m_whisperController->createBackend(
                modelPath,
                useGpu,
                gpuDevice,
                flashAttention
            );
        }

        m_backendName = backendName;
        m_modelPath = modelPath;
        m_useGpu = useGpu;
        m_gpuDevice = gpuDevice;
        m_flashAttention = flashAttention;
    }
    return m_backend.get();
}
