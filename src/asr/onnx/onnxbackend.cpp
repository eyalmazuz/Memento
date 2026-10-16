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

#ifdef HAVE_ASR_BACKEND_ONNX

#include "asr/onnx/onnxbackend.h"

#include <utility>

OnnxBackend::OnnxBackend(OnnxSessionOptions options) :
    m_options(std::move(options))
{
    resetSession();
}

bool OnnxBackend::isLoaded() const noexcept
{
    return m_session != nullptr && m_session->isLoaded();
}

QString OnnxBackend::loadError() const
{
    return m_session != nullptr ? m_session->loadError() : QString{};
}

QFuture<AsrTranscriptionResult> OnnxBackend::transcribe(
    const QString &audioPath,
    AsrTranscriptionOptions options)
{
    auto resultPromise =
        std::make_shared<QPromise<AsrTranscriptionResult>>();
    QFuture<AsrTranscriptionResult> resultFuture = resultPromise->future();
    resultPromise->start();

    if (weak_from_this().expired())
    {
        if (options.failureCallback)
        {
            options.failureCallback(QStringLiteral(
                "ONNX backend is not owned by a shared pointer."
            ));
        }
        resultPromise->addResult(AsrTranscriptionResult::Failed);
        resultPromise->finish();
        return resultFuture;
    }

    transcribeAttempt(audioPath, std::move(options), resultPromise);
    return resultFuture;
}

void OnnxBackend::transcribeAttempt(
    QString audioPath,
    AsrTranscriptionOptions options,
    std::shared_ptr<QPromise<AsrTranscriptionResult>> resultPromise)
{
    std::shared_ptr<OnnxBackend> self = shared_from_this();
    self->m_session->transcribe(audioPath, options).then(
        [self = std::move(self),
         audioPath = std::move(audioPath),
         options = std::move(options),
         resultPromise = std::move(resultPromise)]
        (AsrTranscriptionResult result) mutable
        {
            if (result != AsrTranscriptionResult::Failed ||
                (options.abort && options.abort->load()))
            {
                resultPromise->addResult(result);
                resultPromise->finish();
                return;
            }

            if (self->m_options.useGpu)
            {
                qWarning(
                    "ONNX GPU transcription failed; retrying on CPU."
                );
                self->m_options.useGpu = false;
                self->resetSession();
                self->transcribeAttempt(
                    audioPath,
                    std::move(options),
                    std::move(resultPromise)
                );
                return;
            }

            resultPromise->addResult(result);
            resultPromise->finish();
        }
    );
}

void OnnxBackend::resetSession()
{
    m_session = std::make_unique<OnnxSession>(m_options);
}

#endif // HAVE_ASR_BACKEND_ONNX
