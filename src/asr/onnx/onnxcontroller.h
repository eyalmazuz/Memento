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

#include <memory>
#include <vector>

#include <QObject>
#include <QString>
#include <QUrl>

#ifdef MEMENTO_SYSTEM_QCORO
#include <QCoroTask>
#else
#include <qcoro/qcorotask.h>
#endif

#include "asr/asrbackend.h"

class Context;

/**
 * @brief Result of preparing an ONNX ASR backend.
 */
struct OnnxBackendCreationResult
{
    std::shared_ptr<AsrBackend> backend;
    QString error;
};

/**
 * @brief File download item for a multi-file ONNX model preset.
 */
struct OnnxModelFileSpec
{
    QString relativePath;
    QUrl url;
};

/**
 * @brief Controller handling ONNX ASR model paths, Hugging Face preset downloads,
 *        and backend instantiation.
 */
class OnnxController : public QObject
{
    Q_OBJECT

public:
    explicit OnnxController(Context *context, QObject *parent = nullptr);
    virtual ~OnnxController();

    [[nodiscard]]
    bool isManagedModel(const QString &model) const;

    [[nodiscard]]
    std::vector<OnnxModelFileSpec> modelFiles(const QString &model) const;

    [[nodiscard]]
    bool modelAvailable(const QString &model) const;

    [[nodiscard]]
    QString selectedModelPath() const;

    [[nodiscard]]
    QString modelDirectory(const QString &model) const;

    [[nodiscard]]
    QString modelsDirectory() const;

    [[nodiscard]]
    bool hasAnyModel() const;

    /**
     * @brief Download all files for a managed ONNX model preset.
     */
    QCoro::Task<bool> downloadManagedModel(const QString &model);

    /**
     * @brief Instantiate the ONNX backend for a local model directory.
     */
    QCoro::Task<OnnxBackendCreationResult> createBackend(QString modelPath);

signals:
    void configurationChanged();

private:
    Context *m_context{nullptr};
};
