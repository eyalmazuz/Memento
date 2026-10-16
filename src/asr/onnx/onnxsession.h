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

#include <cstdint>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

#include <QFuture>
#include <QString>

#include <onnxruntime_cxx_api.h>

#include "asr/asrbackend.h"
#include "asr/audio/melfrontend.h"
#include "asr/onnx/onnxmodelconfig.h"
#include "asr/onnx/onnxtokenizer.h"

/**
 * @brief Runtime options for an ONNX ASR session.
 */
struct OnnxSessionOptions
{
    QString modelPath;
    bool useGpu{false};
    int gpuDevice{0};
    int threads{4};
    bool punctuation{true};
};

/**
 * @brief Low-level ONNX Runtime ASR inference engine supporting both
 *        Encoder-Decoder (CohereAsr) and CTC (ParakeetForCTC) architectures.
 */
class OnnxSession
{
public:
    explicit OnnxSession(OnnxSessionOptions options);
    ~OnnxSession();

    [[nodiscard]]
    bool isLoaded() const noexcept
    {
        return m_configValid;
    }

    [[nodiscard]]
    const QString &loadError() const noexcept
    {
        return m_loadError;
    }

    [[nodiscard]]
    QFuture<AsrTranscriptionResult> transcribe(
        const QString &audioPath,
        AsrTranscriptionOptions options
    );

private:
    void initializeSessions();

    AsrTranscriptionResult transcribeSync(
        const QString &audioPath,
        const AsrTranscriptionOptions &options
    );

    AsrTranscriptionResult transcribeEncoderDecoder(
        const std::vector<float> &samples,
        const AsrTranscriptionOptions &options
    );

    AsrTranscriptionResult transcribeCtc(
        const std::vector<float> &samples,
        const AsrTranscriptionOptions &options
    );

    OnnxSessionOptions m_options;
    OnnxModelConfig m_config;
    OnnxTokenizer m_tokenizer;
    std::unique_ptr<AsrAudio::MelFrontend> m_frontend;

    std::unique_ptr<Ort::Session> m_encoderSession;
    std::unique_ptr<Ort::Session> m_decoderSession;
    std::unique_ptr<Ort::Session> m_ctcSession;

    bool m_encoderMaskIsBool{false};
    bool m_ctcMaskIsBool{false};
    int m_numDecoderLayers{4};
    int64_t m_numDecoderHeads{8};
    int64_t m_decoderHeadDim{128};

    bool m_configValid{false};
    bool m_loaded{false};
    QString m_loadError;
    QFuture<void> m_initFuture;
    std::mutex m_mutex;
};

#endif // HAVE_ASR_BACKEND_ONNX
