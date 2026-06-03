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

#pragma once

#include <QObject>

#include <atomic>
#include <memory>

#include <QNetworkAccessManager>
#include <QPointer>
#include <QString>
#include <QUrl>
#include <QVariantMap>

#ifdef MEMENTO_SYSTEM_QCORO
#include <QCoroNetworkReply>
#include <QCoroQmlTask>
#include <QCoroTask>
#else
#include <qcoro/network/qcoronetworkreply.h>
#include <qcoro/qml/qcoroqmltask.h>
#include <qcoro/qcorotask.h>
#endif // MEMENTO_SYSTEM_QCORO

#include "subtitle/subtitleentry.h"

class Context;
class MpvController;
class SubtitleListModel;
class WhisperModel;

/**
 * @brief QML interface for creating subtitles with whisper.cpp.
 */
class WhisperController : public QObject
{
    Q_OBJECT

    Q_PROPERTY(
        bool active
        READ active
        NOTIFY activeChanged
    )

    Q_PROPERTY(
        bool running
        READ running
        NOTIFY runningChanged
    )

    Q_PROPERTY(
        QString currentText
        READ currentText
        NOTIFY currentTextChanged
    )

    Q_PROPERTY(
        bool downloadRunning
        READ downloadRunning
        NOTIFY downloadRunningChanged
    )

    Q_PROPERTY(
        qreal downloadProgress
        READ downloadProgress
        NOTIFY downloadProgressChanged
    )

    Q_PROPERTY(
        qint64 downloadReceived
        READ downloadReceived
        NOTIFY downloadProgressChanged
    )

    Q_PROPERTY(
        qint64 downloadTotal
        READ downloadTotal
        NOTIFY downloadProgressChanged
    )

    Q_PROPERTY(
        qint64 downloadSpeed
        READ downloadSpeed
        NOTIFY downloadProgressChanged
    )

    Q_PROPERTY(
        QString downloadName
        READ downloadName
        NOTIFY downloadNameChanged
    )

public:
    /**
     * @brief Create a WhisperController.
     *
     * @param context The application context.
     * @param parent The parent object.
     */
    explicit WhisperController(Context *context, QObject *parent = nullptr);
    virtual ~WhisperController();

    /**
     * @brief Get if Whisper subtitles are currently selected.
     *
     * @return true if Whisper subtitles are selected,
     * @return false otherwise.
     */
    [[nodiscard]]
    bool active() const noexcept;

    /**
     * @brief Get if Whisper is currently transcribing.
     *
     * @return true if Whisper is transcribing,
     * @return false otherwise.
     */
    [[nodiscard]]
    bool running() const noexcept;

    /**
     * @brief Get the generated subtitle text at the current player position.
     *
     * @return The generated subtitle text.
     */
    [[nodiscard]]
    QString currentText() const;

    /**
     * @brief Get if a managed Whisper model is currently downloading.
     *
     * @return true if a model is downloading,
     * @return false otherwise.
     */
    [[nodiscard]]
    bool downloadRunning() const noexcept;

    /**
     * @brief Get the active download progress from 0.0 to 1.0.
     *
     * @return The download progress.
     */
    [[nodiscard]]
    qreal downloadProgress() const noexcept;

    /**
     * @brief Get the number of bytes downloaded for the active download.
     *
     * @return The received byte count.
     */
    [[nodiscard]]
    qint64 downloadReceived() const noexcept;

    /**
     * @brief Get the total byte count for the active download.
     *
     * @return The total byte count, or 0 if unknown.
     */
    [[nodiscard]]
    qint64 downloadTotal() const noexcept;

    /**
     * @brief Get the average download speed in bytes per second.
     *
     * @return The average download speed.
     */
    [[nodiscard]]
    qint64 downloadSpeed() const noexcept;

    /**
     * @brief Get the active download display name.
     *
     * @return The active download display name.
     */
    [[nodiscard]]
    QString downloadName() const;

    /**
     * @brief Select Whisper subtitles and start transcription.
     *
     * @param controller The player controller to extract audio from.
     * @return A map with "error" on failure.
     */
    Q_INVOKABLE QCoro::QmlTask select(MpvController *controller);

    /**
     * @brief Deselect Whisper subtitles.
     */
    Q_INVOKABLE void stop();

    /**
     * @brief Get the directory used for local Whisper models.
     *
     * @return The absolute model directory path.
     */
    Q_INVOKABLE QString modelsDirectory() const;

    /**
     * @brief Get if the local model directory contains any Whisper model.
     *
     * @return true if at least one model exists,
     * @return false otherwise.
     */
    Q_INVOKABLE bool hasAnyModel() const;

    /**
     * @brief Get if a model preset exists locally.
     *
     * @param model The model preset name.
     * @return true if the model exists,
     * @return false otherwise.
     */
    Q_INVOKABLE bool modelAvailable(const QString &model) const;

    /**
     * @brief Get if the selected model exists locally.
     *
     * @return true if the selected model exists,
     * @return false otherwise.
     */
    Q_INVOKABLE bool selectedModelAvailable() const;

    /**
     * @brief Get if the selected model can be downloaded by Memento.
     *
     * @return true if the selected model is a managed preset,
     * @return false otherwise.
     */
    Q_INVOKABLE bool selectedModelDownloadable() const;

    /**
     * @brief Resolve the selected Whisper model path.
     *
     * @return The selected model path.
     */
    Q_INVOKABLE QString selectedModelPath() const;

    /**
     * @brief Download a managed Whisper model preset.
     *
     * @param model The model preset to download.
     * @return A map with "error" on failure and "path" on success.
     */
    Q_INVOKABLE QCoro::QmlTask downloadModel(const QString &model);

signals:
    /**
     * @brief Emitted when Whisper subtitles are selected or deselected.
     *
     * @param value true if selected, false otherwise.
     */
    void activeChanged(bool value);

    /**
     * @brief Emitted when transcription starts or stops.
     *
     * @param value true if running, false otherwise.
     */
    void runningChanged(bool value);

    /**
     * @brief Emitted when the generated subtitle at the current position
     * changes.
     *
     * @param value The generated subtitle text.
     */
    void currentTextChanged(const QString &value);

    /**
     * @brief Emitted when a managed model download starts or stops.
     *
     * @param value true if a model is downloading, false otherwise.
     */
    void downloadRunningChanged(bool value);

    /**
     * @brief Emitted when active download progress changes.
     */
    void downloadProgressChanged();

    /**
     * @brief Emitted when the active download name changes.
     *
     * @param value The active download name.
     */
    void downloadNameChanged(const QString &value);

private:
    /**
     * @brief Async implementation for select.
     *
     * @param controller The player controller to extract audio from.
     * @return A map with "error" on failure.
     */
    QCoro::Task<QVariantMap> selectAsync(MpvController *controller);

    /**
     * @brief Async implementation for downloadModel.
     *
     * @param model The model preset to download.
     * @return A map with "error" on failure and "path" on success.
     */
    QCoro::Task<QVariantMap> downloadModelAsync(QString model);

    /**
     * @brief Set if Whisper subtitles are currently active.
     *
     * @param value true if active, false otherwise.
     */
    void setActive(bool value);

    /**
     * @brief Set if Whisper is currently transcribing.
     *
     * @param value true if running, false otherwise.
     */
    void setRunning(bool value);

    /**
     * @brief Set the current generated subtitle text.
     *
     * @param value The current subtitle text.
     */
    void setCurrentText(QString value);

    /**
     * @brief Set if a managed model download is running.
     *
     * @param value true if a model is downloading, false otherwise.
     */
    void setDownloadRunning(bool value);

    /**
     * @brief Set the active download name.
     *
     * @param value The active download name.
     */
    void setDownloadName(QString value);

    /**
     * @brief Set active download progress.
     *
     * @param received The downloaded byte count.
     * @param total The total byte count, or 0 if unknown.
     * @param speed The average speed in bytes per second.
     */
    void setDownloadProgress(qint64 received, qint64 total, qint64 speed);

    /**
     * @brief Add a generated subtitle if it belongs to the current run.
     *
     * @param generation The generation the subtitle belongs to.
     * @param segmentGeneration The segment generation the subtitle belongs to.
     * @param subtitle The subtitle to add.
     */
    void addSubtitle(
        int generation,
        int segmentGeneration,
        const SubtitleEntry &subtitle
    );

    /**
     * @brief Handle player position changes while Whisper subtitles are active.
     *
     * @param position The current player position.
     */
    void handlePositionChanged(double position);

    /**
     * @brief Ask the active transcription loop to restart at a position.
     *
     * @param position The requested restart position.
     */
    void requestRestart(double position);

    /**
     * @brief Ask the active transcription loop to reload settings and restart.
     */
    void requestReconfigure();

    /**
     * @brief Consume a pending restart request.
     *
     * @param position Set to the requested restart position.
     * @return true if a restart was pending,
     * @return false otherwise.
     */
    bool consumeRestart(double &position);

    /**
     * @brief Skip a position past any already generated subtitle covering it.
     *
     * @param position The position to test.
     * @return The next uncovered position.
     */
    double skipCoveredPosition(double position) const;

    /**
     * @brief Find the next generated subtitle start after a position.
     *
     * @param position The position to test.
     * @return The next subtitle start, or infinity if none exists.
     */
    double nextSubtitleStart(double position) const;

    /**
     * @brief Update currentText from the current player position.
     */
    void updateCurrentText();

    /**
     * @brief Resolve a user or preset model path.
     *
     * @param path The path to resolve.
     * @return An absolute path.
     */
    QString resolveModelPath(const QString &path) const;

    /**
     * @brief Download a file if it does not already exist.
     *
     * @param url The URL to download.
     * @param path The destination path.
     * @param name The display name for progress UI.
     * @param reportProgress true to update download progress properties.
     * @return true if the file exists or was downloaded, false otherwise.
     */
    QCoro::Task<bool> ensureDownloaded(
        const QUrl &url,
        const QString &path,
        const QString &name = {},
        bool reportProgress = false
    );

    /**
     * @brief Build the managed model filename for a preset.
     *
     * @param model The model preset.
     * @return The managed model filename.
     */
    QString modelFilename(const QString &model) const;

    /**
     * @brief Get if a model name is a managed preset.
     *
     * @param model The model name.
     * @return true if the model is a managed preset,
     * @return false otherwise.
     */
    bool isManagedModel(const QString &model) const;

    /**
     * @brief Build the URL for a managed Whisper model preset.
     *
     * @param model The model preset.
     * @return The download URL.
     */
    QUrl whisperModelUrl(const QString &model) const;

    /**
     * @brief Build the URL for a managed VAD model.
     *
     * @param modelPath The model path or filename.
     * @return The download URL, empty if the model is unmanaged.
     */
    QUrl vadModelUrl(const QString &modelPath) const;

#ifdef MEMENTO_WHISPER_SUPPORT
    /**
     * @brief Get the loaded Whisper model, recreating it if settings changed.
     *
     * @param modelPath The model path.
     * @param useGpu true to try GPU acceleration, false to force CPU.
     * @param gpuDevice The selected GPU device index.
     * @param flashAttention true to use flash attention, false otherwise.
     * @return The Whisper model.
     */
    WhisperModel *model(
        const QString &modelPath,
        bool useGpu,
        int gpuDevice,
        bool flashAttention
    );
#endif // MEMENTO_WHISPER_SUPPORT

    /* The application context */
    Context *m_context{nullptr};

    /* Generated subtitle list model */
    SubtitleListModel *m_subtitles{nullptr};

    /* Connection for tracking player position */
    QMetaObject::Connection m_positionConnection;

    /* Player controller used for the active Whisper subtitle track */
    QPointer<MpvController> m_controller;

    /* true if Whisper subtitles are selected */
    bool m_active{false};

    /* true if transcription is running */
    bool m_running{false};

    /* The subtitle shown for the current player position */
    QString m_currentText;

    /* The network access manager used for downloading managed models */
    QNetworkAccessManager m_manager{this};

    /* true if a managed model download is running */
    bool m_downloadRunning{false};

    /* The active download received byte count */
    qint64 m_downloadReceived{0};

    /* The active download total byte count */
    qint64 m_downloadTotal{0};

    /* The active download average speed in bytes per second */
    qint64 m_downloadSpeed{0};

    /* The active download display name */
    QString m_downloadName;

    /* Incremented to invalidate stale callbacks */
    int m_generation{0};

    /* Incremented to invalidate stale segment callbacks */
    int m_segmentGeneration{0};

    /* Last observed player position */
    double m_lastPosition{-1.0};

    /* true if a transcription restart was requested */
    bool m_restartRequested{false};

    /* true if settings changed and a full reconfiguration is needed */
    bool m_reconfigureRequested{false};

    /* Position of the pending transcription restart */
    double m_restartPosition{0.0};

    /* Cached media path for generated subtitles */
    QString m_cacheMediaPath;

    /* Cached model path for generated subtitles */
    QString m_cacheModelPath;

    /* Cached settings key for generated subtitles */
    QString m_cacheSettingsKey;

#ifdef MEMENTO_WHISPER_SUPPORT
    /* Lazily loaded Whisper model */
    std::unique_ptr<WhisperModel> m_model;

    /* Loaded model path */
    QString m_modelPath;

    /* true if the model was loaded with flash attention */
    bool m_flashAttention{false};

    /* true if the model was loaded with GPU acceleration enabled */
    bool m_useGpu{true};

    /* GPU device index used for the loaded model */
    int m_gpuDevice{0};

    /* Abort flag for the current transcription */
    std::shared_ptr<std::atomic_bool> m_abort;
#endif // MEMENTO_WHISPER_SUPPORT
};
