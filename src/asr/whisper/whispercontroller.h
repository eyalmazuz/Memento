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
#include "asr/asrbackend.h"

class Context;

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

    /**
     * @brief Instantiate the Whisper backend.
     */
    std::unique_ptr<AsrBackend> createBackend(
        const QString &modelPath,
        bool useGpu,
        int gpuDevice,
        bool flashAttention
    );

private:
    Context *m_context{nullptr};
};
