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

WhisperBackend::WhisperBackend(
    const QString &modelPath,
    bool useGpu,
    int gpuDevice,
    bool flashAttention) :
    m_model(modelPath, useGpu, gpuDevice, flashAttention)
{

}

QFuture<AsrTranscriptionResult> WhisperBackend::transcribe(
    const QString &audioPath,
    AsrTranscriptionOptions options)
{
    return m_model.transcribe(audioPath, std::move(options));
}

#endif // MEMENTO_WHISPER_SUPPORT
