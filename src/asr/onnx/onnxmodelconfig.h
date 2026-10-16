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

#include <cstdint>

#include <QString>

#include "asr/audio/melfrontend.h"

/**
 * @brief Supported generic ONNX ASR model architectures.
 */
enum class OnnxAsrArchitecture
{
    Unknown,
    /* Encoder-Decoder autoregressive ASR (e.g., CohereAsr / kitsune-transcribe-t0.6b-ja) */
    EncoderDecoder,
    /* Encoder-only CTC ASR (e.g., ParakeetForCTC / kitsune-transcribe-p0.3b-ja) */
    Ctc,
};

/**
 * @brief Parsed configuration and resolved file paths for an ONNX ASR model directory.
 */
struct OnnxModelConfig
{
    OnnxAsrArchitecture architecture{OnnxAsrArchitecture::Unknown};
    QString modelType;
    QString modelDir;

    /* ONNX graph paths */
    QString encoderOnnxPath;
    QString decoderOnnxPath;
    QString ctcOnnxPath;
    QString tokenizerJsonPath;

    /* Audio frontend parameters */
    AsrAudio::MelFrontendConfig frontend;
    int subsamplingFactor{8};
    double maxAudioClipSeconds{35.0};

    /* Vocabulary & special token IDs */
    int64_t vocabSize{0};
    int64_t eosTokenId{3};
    int64_t padTokenId{2};
    int64_t bosTokenId{4};
    int64_t noSpeechTokenId{1};
    int64_t ctcBlankTokenId{-1};

    /**
     * @brief Load and validate model configuration from a directory or file path.
     *
     * @param path Directory containing config.json / .onnx files, or a file inside it.
     * @param error Populated with a descriptive message if loading fails.
     * @return Parsed OnnxModelConfig.
     */
    [[nodiscard]]
    static OnnxModelConfig loadFromPath(const QString &path, QString &error);

    [[nodiscard]]
    bool isValid() const noexcept
    {
        return architecture != OnnxAsrArchitecture::Unknown &&
            !tokenizerJsonPath.isEmpty();
    }
};
