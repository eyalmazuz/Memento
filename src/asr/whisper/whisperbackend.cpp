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

#ifdef MEMENTO_WHISPER_SUPPORT

#include "asr/whisper/whisperbackend.h"

#include <utility>

#include <QPromise>

WhisperBackend::WhisperBackend(WhisperBackendOptions options) :
    m_options(std::move(options))
{
    resetModel();
}

QFuture<AsrTranscriptionResult> WhisperBackend::transcribe(
    const QString &audioPath,
    AsrTranscriptionOptions options)
{
    auto resultPromise =
        std::make_shared<QPromise<AsrTranscriptionResult>>();
    QFuture<AsrTranscriptionResult> resultFuture = resultPromise->future();
    resultPromise->start();

    if (weak_from_this().expired())
    {
        resultPromise->addResult(AsrTranscriptionResult::Failed);
        resultPromise->finish();
        return resultFuture;
    }

    transcribeAttempt(audioPath, std::move(options), resultPromise);
    return resultFuture;
}

void WhisperBackend::transcribeAttempt(
    QString audioPath,
    AsrTranscriptionOptions options,
    std::shared_ptr<QPromise<AsrTranscriptionResult>> resultPromise)
{
    std::shared_ptr<WhisperBackend> self = shared_from_this();
    WhisperTranscriptionOptions whisperOptions{
        .common = options,
        .threads = self->m_options.threads,
        .bestOf = self->m_options.bestOf,
        .beamSize = self->m_options.beamSize,
        .useVad = self->m_options.useVad,
        .vadModel = self->m_options.vadModel,
    };
    self->m_model->transcribe(audioPath, std::move(whisperOptions)).then(
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

            if (self->m_options.flashAttention)
            {
                qWarning(
                    "Whisper transcription failed with flash attention; "
                    "retrying without flash attention."
                );
                self->m_options.flashAttention = false;
                self->resetModel();
                self->transcribeAttempt(
                    audioPath,
                    std::move(options),
                    std::move(resultPromise)
                );
                return;
            }

            if (self->m_options.useGpu)
            {
                qWarning(
                    "Whisper GPU transcription failed; retrying on CPU/BLAS."
                );
                self->m_options.useGpu = false;
                self->m_options.flashAttention = false;
                self->resetModel();
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

void WhisperBackend::resetModel()
{
    m_model = std::make_unique<WhisperModel>(
        m_options.modelPath,
        m_options.useGpu,
        m_options.gpuDevice,
        m_options.flashAttention
    );
}

#endif // MEMENTO_WHISPER_SUPPORT
