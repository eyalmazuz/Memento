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

#include <atomic>
#include <functional>
#include <memory>

#include <QFuture>
#include <QReadWriteLock>
#include <QString>

#include "subtitle/subtitleentry.h"

struct whisper_context;

/**
 * @brief WhisperModel provides an interface for transcribing audio to subtitles.
 */
class WhisperModel
{
public:
    /**
     * @brief Result of a transcription request.
     */
    enum class TranscriptionResult
    {
        Success,
        Failed,
        Canceled,
    };

    /**
     * @brief Runtime transcription options.
     */
    struct Options
    {
        /* Number of inference threads */
        int threads{1};

        /* Number of greedy candidates */
        int bestOf{5};

        /* Beam search size */
        int beamSize{5};

        /* true to use VAD */
        bool useVad{false};

        /* VAD model path */
        QString vadModel;

        /* Abort flag */
        std::shared_ptr<std::atomic_bool> abort;

        /* Called for each generated segment */
        std::function<void(const SubtitleEntry &)> segmentCallback;
    };

    /**
     * @brief Initializes a WhisperModel object.
     *
     * @param modelPath The local whisper.cpp model path.
     * @param useGpu true to try GPU acceleration, false to force CPU.
     * @param gpuDevice The selected GPU device index.
     * @param flashAttention true to enable flash attention, false otherwise.
     */
    WhisperModel(
        const QString &modelPath,
        bool useGpu,
        int gpuDevice,
        bool flashAttention
    );
    virtual ~WhisperModel();

    /**
     * @brief Transcribe a Whisper-ready audio file.
     *
     * @param audioPath The path to a 16 kHz mono PCM16 WAV audio file.
     * @param options Runtime transcription options.
     * @return A future containing the transcription result.
     */
    QFuture<TranscriptionResult> transcribe(
        const QString &audioPath, Options options);

private:
    /**
     * @brief Get the underlying whisper.cpp context.
     *
     * @return A pointer to the context.
     */
    inline whisper_context *getModel() const;

    /* A future containing the model. This prevents model loading from blocking
     * the UI thread. */
    QFuture<whisper_context *> m_model;

    /* A lock to prevent the model from being deleted while it is in use. */
    QReadWriteLock m_modelLock;
};

#endif // MEMENTO_WHISPER_SUPPORT
