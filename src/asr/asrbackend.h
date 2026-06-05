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

#include <atomic>
#include <functional>
#include <memory>

#include <QFuture>
#include <QString>

#include "subtitle/subtitleentry.h"

/**
 * @brief Result of an ASR transcription request.
 */
enum class AsrTranscriptionResult
{
    Success,
    Failed,
    Canceled,
};

/**
 * @brief Runtime options shared by ASR backends.
 */
struct AsrTranscriptionOptions
{
    /* Number of inference threads */
    int threads{1};

    /* Number of greedy candidates */
    int bestOf{5};

    /* Beam search size */
    int beamSize{5};

    /* true to use VAD if the backend supports it */
    bool useVad{false};

    /* VAD model path */
    QString vadModel;

    /* Backend language hint */
    QString language;

    /* Abort flag */
    std::shared_ptr<std::atomic_bool> abort;

    /* Called for each generated subtitle segment */
    std::function<void(const SubtitleEntry &)> segmentCallback;
};

/**
 * @brief Common interface for subtitle-generating ASR backends.
 */
class AsrBackend
{
public:
    virtual ~AsrBackend() = default;

    /**
     * @brief Transcribe a backend-ready audio file.
     *
     * @param audioPath The audio file path.
     * @param options Runtime transcription options.
     * @return A future containing the transcription result.
     */
    [[nodiscard]]
    virtual QFuture<AsrTranscriptionResult> transcribe(
        const QString &audioPath,
        AsrTranscriptionOptions options
    ) = 0;
};
