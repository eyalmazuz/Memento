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

#ifdef HAVE_ASR_BACKEND_ONNX

#include <memory>

#include <QPromise>
#include <QString>

#include "asr/asrbackend.h"
#include "asr/onnx/onnxsession.h"

/**
 * @brief ASR backend powered by ONNX Runtime (supporting CohereAsr, Parakeet CTC, etc.).
 */
class OnnxBackend final :
    public AsrBackend,
    public std::enable_shared_from_this<OnnxBackend>
{
public:
    explicit OnnxBackend(OnnxSessionOptions options);

    [[nodiscard]]
    bool isLoaded() const noexcept;

    [[nodiscard]]
    QString loadError() const;

    [[nodiscard]]
    QFuture<AsrTranscriptionResult> transcribe(
        const QString &audioPath,
        AsrTranscriptionOptions options
    ) override;

private:
    void transcribeAttempt(
        QString audioPath,
        AsrTranscriptionOptions options,
        std::shared_ptr<QPromise<AsrTranscriptionResult>> resultPromise
    );

    void resetSession();

    OnnxSessionOptions m_options;
    std::unique_ptr<OnnxSession> m_session;
};

#endif // HAVE_ASR_BACKEND_ONNX
