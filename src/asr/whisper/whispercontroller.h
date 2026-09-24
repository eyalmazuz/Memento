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
#include <QString>
#include <QStringList>
#include <QUrl>
#include <memory>

#ifdef MEMENTO_SYSTEM_QCORO
#include <QCoroTask>
#else
#include <qcoro/qcorotask.h>
#endif

#include "asr/asrbackend.h"

class Context;

/**
 * @brief Result of preparing a Whisper backend and its required assets.
 */
struct WhisperBackendCreationResult
{
    std::shared_ptr<AsrBackend> backend;
    QString error;
};

/**
 * @brief Controller handling Whisper model paths, download URLs, and backend creation.
 */
class WhisperController : public QObject
{
    Q_OBJECT

public:
    explicit WhisperController(Context *context, QObject *parent = nullptr);
    virtual ~WhisperController();

    [[nodiscard]]
    bool isManagedModel(const QString &model) const;

    [[nodiscard]]
    QString modelFilename(const QString &model) const;

    [[nodiscard]]
    QUrl whisperModelUrl(const QString &model) const;

    [[nodiscard]]
    QUrl vadModelUrl(const QString &modelPath) const;

    [[nodiscard]]
    bool modelAvailable(const QString &model) const;

    [[nodiscard]]
    QString selectedModelPath() const;

    [[nodiscard]]
    QString resolveModelPath(
        const QString &path,
        const QStringList &suffixes
    ) const;

    [[nodiscard]]
    QString modelsDirectory() const;

    [[nodiscard]]
    bool hasAnyModel() const;

    /**
     * @brief Prepare required Whisper assets and instantiate the backend.
     *
     * @param modelPath The local Whisper model path.
     * @return The backend, or a localized setup error.
     */
    QCoro::Task<WhisperBackendCreationResult> createBackend(
        QString modelPath);

signals:
    /**
     * @brief Emitted when a Whisper-specific setting changes.
     */
    void configurationChanged();

private:
    Context *m_context{nullptr};
};
