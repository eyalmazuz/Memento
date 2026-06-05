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
#include <QElapsedTimer>
#include <QPointer>
#include <QString>
#include <QStringList>
#include <QUrl>
#include <QVariantMap>

#ifdef MEMENTO_SYSTEM_QCORO
#include <QCoroQmlTask>
#include <QCoroTask>
#else
#include <qcoro/qml/qcoroqmltask.h>
#include <qcoro/qcorotask.h>
#endif

#include "subtitle/subtitleentry.h"
#include "asr/asrbackend.h"

class Context;
class MpvController;
class SubtitleListModel;
class DownloadManager;
class WhisperController;

/**
 * @brief QML interface coordinating multiple ASR backends and model downloading.
 */
class AsrController : public QObject
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
    explicit AsrController(Context *context, QObject *parent = nullptr);
    virtual ~AsrController();

    [[nodiscard]]
    bool active() const noexcept;

    [[nodiscard]]
    bool running() const noexcept;

    [[nodiscard]]
    QString currentText() const;

    [[nodiscard]]
    bool downloadRunning() const noexcept;

    [[nodiscard]]
    qreal downloadProgress() const noexcept;

    [[nodiscard]]
    qint64 downloadReceived() const noexcept;

    [[nodiscard]]
    qint64 downloadTotal() const noexcept;

    [[nodiscard]]
    qint64 downloadSpeed() const noexcept;

    [[nodiscard]]
    QString downloadName() const;

    /**
     * @brief Select ASR subtitles and start transcription.
     */
    Q_INVOKABLE QCoro::QmlTask select(MpvController *controller);

    /**
     * @brief Deselect ASR subtitles.
     */
    Q_INVOKABLE void stop();

    /**
     * @brief Get the directory used for local ASR models.
     */
    Q_INVOKABLE QString modelsDirectory() const;

    /**
     * @brief Get if the local model directory contains any ASR model.
     */
    Q_INVOKABLE bool hasAnyModel() const;

    /**
     * @brief Get if a model preset exists locally.
     */
    Q_INVOKABLE bool modelAvailable(const QString &model) const;


    /**
     * @brief Get if the selected model exists locally.
     */
    Q_INVOKABLE bool selectedModelAvailable() const;

    /**
     * @brief Get if the selected model can be downloaded.
     */
    Q_INVOKABLE bool selectedModelDownloadable() const;

    /**
     * @brief Download a model preset.
     */
    Q_INVOKABLE QCoro::QmlTask downloadModel(const QString &model);

    QCoro::Task<QVariantMap> downloadModelAsync(QString model);

signals:
    void activeChanged();
    void runningChanged();
    void currentTextChanged();
    void downloadRunningChanged();
    void downloadProgressChanged();
    void downloadNameChanged();

private:
    void setActive(bool value);
    void setRunning(bool value);
    void setCurrentText(QString value);

    QCoro::Task<QVariantMap> selectAsync(MpvController *controller);
    void handlePositionChanged(double position);
    void requestRestart(double position);
    void requestReconfigure();
    bool consumeRestart(double &position);
    double skipCoveredPosition(double position) const;
    double nextSubtitleStart(double position) const;
    void addSubtitle(int generation, int segmentGeneration, const SubtitleEntry &entry);
    void updateCurrentText();

    QString selectedModelPath() const;
    AsrBackend *backend(
        const QString &backendName,
        const QString &modelPath,
        bool useGpu,
        int gpuDevice,
        bool flashAttention
    );

    Context *m_context{nullptr};
    DownloadManager *m_downloader{nullptr};
    WhisperController *m_whisperController{nullptr};

    std::unique_ptr<AsrBackend> m_backend;
    QString m_backendName;
    QString m_modelPath;
    bool m_useGpu{false};
    int m_gpuDevice{0};
    bool m_flashAttention{false};

    SubtitleListModel *m_subtitles{nullptr};
    QPointer<MpvController> m_controller;
    QMetaObject::Connection m_positionConnection;

    bool m_active{false};
    bool m_running{false};
    QString m_currentText;

    QString m_cacheMediaPath;
    QString m_cacheModelPath;
    QString m_cacheSettingsKey;

    std::shared_ptr<std::atomic_bool> m_abort;
    int m_generation{0};
    int m_segmentGeneration{0};
    int m_selectGeneration{0};

    std::atomic<double> m_restartPosition{0.0};
    std::atomic_bool m_restartRequested{false};
    std::atomic_bool m_reconfigureRequested{false};

    double m_lastPosition{0.0};
    QElapsedTimer m_lastPositionTimer;
};
