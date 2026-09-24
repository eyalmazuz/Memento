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

#ifdef MEMENTO_WHISPER_SUPPORT

#include <memory>
#include <QPromise>
#include <QString>

#include "asr/asrbackend.h"
#include "asr/whisper/whispermodel.h"

/**
 * @brief Configuration owned by the Whisper implementation.
 */
struct WhisperBackendOptions
{
    /* Local whisper.cpp model */
    QString modelPath;

    /* Try GPU acceleration when true */
    bool useGpu{false};

    /* Selected GPU device */
    int gpuDevice{0};

    /* Enable flash attention when true */
    bool flashAttention{false};

    /* Number of inference threads */
    int threads{1};

    /* Number of greedy candidates */
    int bestOf{5};

    /* Beam search size */
    int beamSize{5};

    /* Enable voice activity detection when true */
    bool useVad{false};

    /* VAD model path */
    QString vadModel;
};

/**
 * @brief ASR backend powered by whisper.cpp.
 */
class WhisperBackend final :
    public AsrBackend,
    public std::enable_shared_from_this<WhisperBackend>
{
public:
    /**
     * @brief Create a WhisperBackend.
     *
     * @param options Whisper-specific backend configuration.
     */
    explicit WhisperBackend(WhisperBackendOptions options);

    QFuture<AsrTranscriptionResult> transcribe(
        const QString &audioPath,
        AsrTranscriptionOptions options
    ) override;

private:
    /**
     * @brief Run a Whisper attempt and retry with fallback configuration.
     *
     * @param audioPath Path to the extracted audio clip.
     * @param options Shared request options.
     * @return A future containing the final transcription result.
     */
    void transcribeAttempt(
        QString audioPath,
        AsrTranscriptionOptions options,
        std::shared_ptr<QPromise<AsrTranscriptionResult>> resultPromise);

    /**
     * @brief Recreate the low-level model with the current fallback options.
     */
    void resetModel();

    /* Whisper-specific transcription and model settings */
    WhisperBackendOptions m_options;

    /* Low-level whisper.cpp model wrapper */
    std::unique_ptr<WhisperModel> m_model;
};

#endif // MEMENTO_WHISPER_SUPPORT
