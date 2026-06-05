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

#include "asr/asrbackend.h"
#include "asr/whisper/whispermodel.h"

/**
 * @brief ASR backend powered by whisper.cpp.
 */
class WhisperBackend final : public AsrBackend
{
public:
    /**
     * @brief Create a WhisperBackend.
     *
     * @param modelPath The local whisper.cpp model path.
     * @param useGpu true to try GPU acceleration, false to force CPU.
     * @param gpuDevice The selected GPU device index.
     * @param flashAttention true to enable flash attention, false otherwise.
     */
    WhisperBackend(
        const QString &modelPath,
        bool useGpu,
        int gpuDevice,
        bool flashAttention
    );

    QFuture<AsrTranscriptionResult> transcribe(
        const QString &audioPath,
        AsrTranscriptionOptions options
    ) override;

private:
    /* Low-level whisper.cpp model wrapper */
    WhisperModel m_model;
};

#endif // MEMENTO_WHISPER_SUPPORT
