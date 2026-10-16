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

#include "asr/onnx/onnxsession.h"

#include <algorithm>
#include <cmath>
#include <utility>

#include <QtConcurrent>

#include "asr/audio/wavreader.h"

namespace
{

Ort::Env &sharedOrtEnv()
{
    static Ort::Env env(ORT_LOGGING_LEVEL_WARNING, "MementoOnnxAsr");
    return env;
}

/**
 * @brief Detect 24-token periodic repetition in generated token sequence
 *        (matching Multy123/kitsune-transcribe-t0.6b-ja repetition stop rule).
 */
bool hasPeriodicRepetition24(const std::vector<int64_t> &tokens)
{
    constexpr size_t WINDOW = 24;
    if (tokens.size() < WINDOW)
    {
        return false;
    }

    const int64_t *tail = tokens.data() + (tokens.size() - WINDOW);
    for (size_t period = 1; period <= WINDOW / 2; ++period)
    {
        bool periodic = true;
        for (size_t i = period; i < WINDOW; ++i)
        {
            if (tail[i] != tail[i - period])
            {
                periodic = false;
                break;
            }
        }
        if (periodic)
        {
            return true;
        }
    }
    return false;
}

bool endsWithJapaneseSentencePunctuation(const QString &text)
{
    if (text.isEmpty())
    {
        return false;
    }
    const QChar last = text.back();
    return last == QChar(0x3002) || // 。
           last == QChar(0xFF01) || // ！
           last == QChar(0xFF1F) || // ？
           last == QChar(0x2026) || // …
           last == QLatin1Char('!') ||
           last == QLatin1Char('?');
}

} // namespace

OnnxSession::OnnxSession(OnnxSessionOptions options) :
    m_options(std::move(options))
{
    QString cfgError;
    m_config = OnnxModelConfig::loadFromPath(m_options.modelPath, cfgError);
    if (!m_config.isValid())
    {
        m_loadError = cfgError.isEmpty()
            ? QStringLiteral("Invalid ONNX ASR model directory: %1").arg(m_options.modelPath)
            : cfgError;
        qWarning("%s", qUtf8Printable(m_loadError));
        return;
    }

    QString tokError;
    if (!m_tokenizer.load(m_config.tokenizerJsonPath, tokError))
    {
        m_loadError = tokError;
        qWarning("%s", qUtf8Printable(m_loadError));
        return;
    }

    m_configValid = true;
    m_initFuture = QtConcurrent::run([this]() { initializeSessions(); });
}

OnnxSession::~OnnxSession()
{
    if (m_initFuture.isValid())
    {
        m_initFuture.waitForFinished();
    }
}

void OnnxSession::initializeSessions()
{
    try
    {
        Ort::Env &env = sharedOrtEnv();
        Ort::SessionOptions sessionOpts;
        const int numThreads = m_options.threads > 0 ? m_options.threads : 0;
        sessionOpts.SetIntraOpNumThreads(numThreads);
        sessionOpts.SetInterOpNumThreads(1);
        sessionOpts.SetGraphOptimizationLevel(GraphOptimizationLevel::ORT_ENABLE_ALL);

        if (m_options.useGpu)
        {
            const std::vector<std::string> providers = Ort::GetAvailableProviders();
            const bool hasCuda = std::find(
                providers.begin(),
                providers.end(),
                "CUDAExecutionProvider"
            ) != providers.end();
            if (hasCuda)
            {
                try
                {
                    OrtCUDAProviderOptions cudaOpts{};
                    cudaOpts.device_id = std::max(0, m_options.gpuDevice);
                    sessionOpts.AppendExecutionProvider_CUDA(cudaOpts);
                }
                catch (const Ort::Exception &cudaEx)
                {
                    qWarning(
                        "ONNX Runtime CUDA Execution Provider unavailable (%s); falling back to CPU.",
                        cudaEx.what()
                    );
                }
            }
            else
            {
                qWarning(
                    "ONNX Runtime library was built without CUDA Execution Provider; using CPU."
                );
            }
        }

        Ort::AllocatorWithDefaultOptions allocator;

        if (m_config.architecture == OnnxAsrArchitecture::EncoderDecoder)
        {
#ifdef _WIN32
            const std::wstring encPathW = m_config.encoderOnnxPath.toStdWString();
            const std::wstring decPathW = m_config.decoderOnnxPath.toStdWString();
            m_encoderSession = std::make_unique<Ort::Session>(
                env, encPathW.c_str(), sessionOpts
            );
            m_decoderSession = std::make_unique<Ort::Session>(
                env, decPathW.c_str(), sessionOpts
            );
#else
            const QByteArray encPathUtf8 = m_config.encoderOnnxPath.toUtf8();
            const QByteArray decPathUtf8 = m_config.decoderOnnxPath.toUtf8();
            m_encoderSession = std::make_unique<Ort::Session>(
                env, encPathUtf8.constData(), sessionOpts
            );
            m_decoderSession = std::make_unique<Ort::Session>(
                env, decPathUtf8.constData(), sessionOpts
            );
#endif
            // Inspect encoder inputs for mel bins and attention_mask element type
            const size_t encInputCount = m_encoderSession->GetInputCount();
            for (size_t i = 0; i < encInputCount; ++i)
            {
                const auto nameAlloc = m_encoderSession->GetInputNameAllocated(i, allocator);
                const std::string name(nameAlloc.get());
                const Ort::TypeInfo inputTypeInfo = m_encoderSession->GetInputTypeInfo(i);
                const auto typeInfo = inputTypeInfo.GetTensorTypeAndShapeInfo();
                const auto shape = typeInfo.GetShape();
                if (name == "input_features" && shape.size() == 3 && shape[2] > 0)
                {
                    m_config.frontend.nMels = static_cast<int>(shape[2]);
                }
                else if (name == "attention_mask")
                {
                    m_encoderMaskIsBool =
                        (typeInfo.GetElementType() == ONNX_TENSOR_ELEMENT_DATA_TYPE_BOOL);
                }
            }

            // Inspect decoder inputs to determine layer count and KV cache dimensions dynamically
            int maxLayerIdx = -1;
            const size_t decInputCount = m_decoderSession->GetInputCount();
            for (size_t i = 0; i < decInputCount; ++i)
            {
                const auto nameAlloc = m_decoderSession->GetInputNameAllocated(i, allocator);
                const std::string name(nameAlloc.get());
                if (name.rfind("past_key.", 0) == 0)
                {
                    const int layerIdx = std::atoi(name.c_str() + 9);
                    maxLayerIdx = std::max(maxLayerIdx, layerIdx);
                    const Ort::TypeInfo decInputTypeInfo = m_decoderSession->GetInputTypeInfo(i);
                    const auto shape =
                        decInputTypeInfo.GetTensorTypeAndShapeInfo().GetShape();
                    if (shape.size() == 4)
                    {
                        if (shape[1] > 0)
                        {
                            m_numDecoderHeads = shape[1];
                        }
                        if (shape[3] > 0)
                        {
                            m_decoderHeadDim = shape[3];
                        }
                    }
                }
            }
            if (maxLayerIdx >= 0)
            {
                m_numDecoderLayers = maxLayerIdx + 1;
            }
        }
        else if (m_config.architecture == OnnxAsrArchitecture::Ctc)
        {
#ifdef _WIN32
            const std::wstring ctcPathW = m_config.ctcOnnxPath.toStdWString();
            m_ctcSession = std::make_unique<Ort::Session>(
                env, ctcPathW.c_str(), sessionOpts
            );
#else
            const QByteArray ctcPathUtf8 = m_config.ctcOnnxPath.toUtf8();
            m_ctcSession = std::make_unique<Ort::Session>(
                env, ctcPathUtf8.constData(), sessionOpts
            );
#endif
            const size_t ctcInputCount = m_ctcSession->GetInputCount();
            for (size_t i = 0; i < ctcInputCount; ++i)
            {
                const auto nameAlloc = m_ctcSession->GetInputNameAllocated(i, allocator);
                const std::string name(nameAlloc.get());
                const Ort::TypeInfo ctcInputTypeInfo = m_ctcSession->GetInputTypeInfo(i);
                const auto typeInfo = ctcInputTypeInfo.GetTensorTypeAndShapeInfo();
                const auto shape = typeInfo.GetShape();
                if (name == "input_features" && shape.size() == 3 && shape[2] > 0)
                {
                    m_config.frontend.nMels = static_cast<int>(shape[2]);
                }
                else if (name == "attention_mask")
                {
                    m_ctcMaskIsBool =
                        (typeInfo.GetElementType() == ONNX_TENSOR_ELEMENT_DATA_TYPE_BOOL);
                }
            }
        }

        m_frontend = std::make_unique<AsrAudio::MelFrontend>(m_config.frontend);
        m_loaded = true;
    }
    catch (const std::exception &ex)
    {
        m_loadError = QStringLiteral("ONNX Runtime initialization failed: %1")
            .arg(QString::fromUtf8(ex.what()));
        qWarning("%s", qUtf8Printable(m_loadError));
    }
}

QFuture<AsrTranscriptionResult> OnnxSession::transcribe(
    const QString &audioPath,
    AsrTranscriptionOptions options)
{
    return QtConcurrent::run(
        [this, audioPath, options = std::move(options)]() -> AsrTranscriptionResult
        {
            return transcribeSync(audioPath, options);
        }
    );
}

AsrTranscriptionResult OnnxSession::transcribeSync(
    const QString &audioPath,
    const AsrTranscriptionOptions &options)
{
    const auto fail = [&options](const QString &message)
    {
        if (options.failureCallback)
        {
            options.failureCallback(message);
        }
    };

    if (options.abort && options.abort->load())
    {
        return AsrTranscriptionResult::Canceled;
    }

    QString wavError;
    const std::vector<float> samples = AsrAudio::readPcm16Wav(
        audioPath,
        wavError,
        m_config.frontend.sampleRate
    );
    if (samples.empty())
    {
        fail(wavError.isEmpty()
            ? QStringLiteral("Could not read extracted WAV audio.")
            : wavError);
        return AsrTranscriptionResult::Failed;
    }

    if (options.abort && options.abort->load())
    {
        return AsrTranscriptionResult::Canceled;
    }

    if (m_initFuture.isValid())
    {
        m_initFuture.waitForFinished();
    }

    if (options.abort && options.abort->load())
    {
        return AsrTranscriptionResult::Canceled;
    }

    if (!m_loaded || !m_frontend)
    {
        fail(m_loadError.isEmpty()
            ? QStringLiteral("ONNX ASR model is not loaded.")
            : m_loadError);
        return AsrTranscriptionResult::Failed;
    }

    if (options.abort && options.abort->load())
    {
        return AsrTranscriptionResult::Canceled;
    }

    std::scoped_lock lock(m_mutex);
    try
    {
        if (m_config.architecture == OnnxAsrArchitecture::EncoderDecoder)
        {
            return transcribeEncoderDecoder(samples, options);
        }
        if (m_config.architecture == OnnxAsrArchitecture::Ctc)
        {
            return transcribeCtc(samples, options);
        }
        fail(QStringLiteral("Unsupported ONNX ASR model architecture."));
        return AsrTranscriptionResult::Failed;
    }
    catch (const std::exception &ex)
    {
        const QString err = QStringLiteral("ONNX inference error: %1")
            .arg(QString::fromUtf8(ex.what()));
        qWarning("%s", qUtf8Printable(err));
        fail(err);
        return AsrTranscriptionResult::Failed;
    }
}

AsrTranscriptionResult OnnxSession::transcribeEncoderDecoder(
    const std::vector<float> &samples,
    const AsrTranscriptionOptions &options)
{
    // Segment the 30s audio window into speech utterances so untimed seq2seq models
    // (which use <|notimestamp|>) emit accurately timed subtitle cues.
    std::vector<AsrAudio::AudioUtteranceSpan> utterances =
        AsrAudio::MelFrontend::segmentSpeechUtterances(
            samples.data(),
            samples.size(),
            m_config.frontend.sampleRate,
            10.0
        );

    if (utterances.empty())
    {
        // Silence clip — nothing to emit, return Success.
        return AsrTranscriptionResult::Success;
    }

    Ort::MemoryInfo memInfo = Ort::MemoryInfo::CreateCpu(
        OrtArenaAllocator,
        OrtMemTypeDefault
    );

    const std::vector<int64_t> promptIds = m_tokenizer.buildCohereAsrPrompt(
        options.language,
        m_options.punctuation
    );

    // Prepare encoder output names: ["encoder_attention_mask", "cross_key.0", "cross_value.0", ...]
    std::vector<std::string> encOutNameStrs;
    encOutNameStrs.reserve(static_cast<size_t>(1 + 2 * m_numDecoderLayers));
    encOutNameStrs.emplace_back("encoder_attention_mask");
    for (int l = 0; l < m_numDecoderLayers; ++l)
    {
        encOutNameStrs.push_back("cross_key." + std::to_string(l));
        encOutNameStrs.push_back("cross_value." + std::to_string(l));
    }
    std::vector<const char *> encOutNames;
    encOutNames.reserve(encOutNameStrs.size());
    for (const std::string &s : encOutNameStrs)
    {
        encOutNames.push_back(s.c_str());
    }

    // Prepare decoder input/output names
    std::vector<std::string> decInNameStrs;
    decInNameStrs.reserve(static_cast<size_t>(2 + 4 * m_numDecoderLayers));
    decInNameStrs.emplace_back("input_ids");
    decInNameStrs.emplace_back("encoder_attention_mask");
    for (int l = 0; l < m_numDecoderLayers; ++l)
    {
        decInNameStrs.push_back("cross_key." + std::to_string(l));
        decInNameStrs.push_back("cross_value." + std::to_string(l));
    }
    for (int l = 0; l < m_numDecoderLayers; ++l)
    {
        decInNameStrs.push_back("past_key." + std::to_string(l));
        decInNameStrs.push_back("past_value." + std::to_string(l));
    }
    std::vector<const char *> decInNames;
    decInNames.reserve(decInNameStrs.size());
    for (const std::string &s : decInNameStrs)
    {
        decInNames.push_back(s.c_str());
    }

    std::vector<std::string> decOutNameStrs;
    decOutNameStrs.reserve(static_cast<size_t>(1 + 2 * m_numDecoderLayers));
    decOutNameStrs.emplace_back("logits");
    for (int l = 0; l < m_numDecoderLayers; ++l)
    {
        decOutNameStrs.push_back("present_key." + std::to_string(l));
        decOutNameStrs.push_back("present_value." + std::to_string(l));
    }
    std::vector<const char *> decOutNames;
    decOutNames.reserve(decOutNameStrs.size());
    for (const std::string &s : decOutNameStrs)
    {
        decOutNames.push_back(s.c_str());
    }

    const char *encInNames[2] = {"input_features", "attention_mask"};

    for (const AsrAudio::AudioUtteranceSpan &utt : utterances)
    {
        if (options.abort && options.abort->load())
        {
            return AsrTranscriptionResult::Canceled;
        }

        const size_t uttLen = utt.endSample - utt.startSample;
        AsrAudio::MelFeatureOutput mel = m_frontend->extract(
            samples.data() + utt.startSample,
            uttLen
        );
        if (mel.numFrames <= 0 || mel.validFrames <= 0)
        {
            continue;
        }

        const int64_t featShape[3] = {1, mel.numFrames, mel.nMels};
        const int64_t maskShape[2] = {1, mel.numFrames};

        std::vector<Ort::Value> encInputs;
        encInputs.reserve(2);
        encInputs.push_back(Ort::Value::CreateTensor<float>(
            memInfo,
            mel.features.data(),
            mel.features.size(),
            featShape,
            3
        ));
        if (m_encoderMaskIsBool)
        {
            encInputs.push_back(Ort::Value::CreateTensor<bool>(
                memInfo,
                reinterpret_cast<bool *>(mel.attentionMaskBool.data()),
                mel.attentionMaskBool.size(),
                maskShape,
                2
            ));
        }
        else
        {
            encInputs.push_back(Ort::Value::CreateTensor<int64_t>(
                memInfo,
                mel.attentionMaskInt64.data(),
                mel.attentionMaskInt64.size(),
                maskShape,
                2
            ));
        }

        std::vector<Ort::Value> encOutputs = m_encoderSession->Run(
            Ort::RunOptions{nullptr},
            encInNames,
            encInputs.data(),
            encInputs.size(),
            encOutNames.data(),
            encOutNames.size()
        );

        if (options.abort && options.abort->load())
        {
            return AsrTranscriptionResult::Canceled;
        }

        // Initialize empty self-attention KV cache [1, numHeads, 0, headDim]
        const int64_t emptyKvShape[4] = {1, m_numDecoderHeads, 0, m_decoderHeadDim};
        std::vector<Ort::Value> pastKv;
        pastKv.reserve(static_cast<size_t>(2 * m_numDecoderLayers));
        for (int l = 0; l < 2 * m_numDecoderLayers; ++l)
        {
            pastKv.push_back(Ort::Value::CreateTensor<float>(
                memInfo,
                nullptr,
                0,
                emptyKvShape,
                4
            ));
        }

        const double uttDurationSec = std::max(0.1, utt.endSec - utt.startSec);
        const int maxNewTokens = 16 + static_cast<int>(
            10.0 * std::min(uttDurationSec, m_config.maxAudioClipSeconds)
        );

        std::vector<int64_t> stepIds = promptIds;
        std::vector<int64_t> generatedTokens;
        generatedTokens.reserve(static_cast<size_t>(maxNewTokens));

        for (int step = 0; step < maxNewTokens; ++step)
        {
            if (options.abort && options.abort->load())
            {
                return AsrTranscriptionResult::Canceled;
            }

            const int64_t idsShape[2] = {1, static_cast<int64_t>(stepIds.size())};
            std::vector<Ort::Value> decInputs;
            decInputs.reserve(decInNames.size());
            decInputs.push_back(Ort::Value::CreateTensor<int64_t>(
                memInfo,
                stepIds.data(),
                stepIds.size(),
                idsShape,
                2
            ));

            // Share encoder_attention_mask and cross_key/cross_value without copying buffer data
            for (size_t i = 0; i < encOutputs.size(); ++i)
            {
                Ort::Value &src = encOutputs[i];
                const auto info = src.GetTensorTypeAndShapeInfo();
                const auto shape = info.GetShape();
                if (info.GetElementType() == ONNX_TENSOR_ELEMENT_DATA_TYPE_BOOL)
                {
                    decInputs.push_back(Ort::Value::CreateTensor<bool>(
                        memInfo,
                        src.GetTensorMutableData<bool>(),
                        info.GetElementCount(),
                        shape.data(),
                        shape.size()
                    ));
                }
                else if (info.GetElementType() == ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64)
                {
                    decInputs.push_back(Ort::Value::CreateTensor<int64_t>(
                        memInfo,
                        src.GetTensorMutableData<int64_t>(),
                        info.GetElementCount(),
                        shape.data(),
                        shape.size()
                    ));
                }
                else
                {
                    decInputs.push_back(Ort::Value::CreateTensor<float>(
                        memInfo,
                        src.GetTensorMutableData<float>(),
                        info.GetElementCount(),
                        shape.data(),
                        shape.size()
                    ));
                }
            }

            for (Ort::Value &kv : pastKv)
            {
                decInputs.push_back(std::move(kv));
            }

            std::vector<Ort::Value> decOutputs = m_decoderSession->Run(
                Ort::RunOptions{nullptr},
                decInNames.data(),
                decInputs.data(),
                decInputs.size(),
                decOutNames.data(),
                decOutNames.size()
            );

            // Extract logits [1, seqLen, vocabSize] and find argmax of last position
            const Ort::Value &logitsVal = decOutputs[0];
            const auto logitsShape = logitsVal.GetTensorTypeAndShapeInfo().GetShape();
            const int64_t seqLen = logitsShape.at(1);
            const int64_t vocabSize = logitsShape.at(2);
            const float *lastLogits =
                logitsVal.GetTensorData<float>() + (seqLen - 1) * vocabSize;

            int64_t nextToken = 0;
            float maxLogit = lastLogits[0];
            for (int64_t v = 1; v < vocabSize; ++v)
            {
                if (lastLogits[v] > maxLogit)
                {
                    maxLogit = lastLogits[v];
                    nextToken = v;
                }
            }

            if (nextToken == m_config.eosTokenId ||
                nextToken == m_config.noSpeechTokenId)
            {
                break;
            }

            generatedTokens.push_back(nextToken);
            if (hasPeriodicRepetition24(generatedTokens))
            {
                break;
            }

            // Move present_key / present_value into pastKv for the next step
            pastKv.clear();
            for (size_t i = 1; i < decOutputs.size(); ++i)
            {
                pastKv.push_back(std::move(decOutputs[i]));
            }
            stepIds.assign(1, nextToken);
        }

        if (!generatedTokens.empty())
        {
            const QString text = m_tokenizer.decode(generatedTokens, true).trimmed();
            if (!text.isEmpty() && options.segmentCallback)
            {
                options.segmentCallback(SubtitleEntry{
                    .text = text,
                    .start = utt.startSec,
                    .end = utt.endSec,
                });
            }
        }
    }

    return AsrTranscriptionResult::Success;
}

AsrTranscriptionResult OnnxSession::transcribeCtc(
    const std::vector<float> &samples,
    const AsrTranscriptionOptions &options)
{
    AsrAudio::MelFeatureOutput mel = m_frontend->extract(samples);
    if (mel.numFrames <= 0 || mel.validFrames <= 0)
    {
        return AsrTranscriptionResult::Success;
    }

    Ort::MemoryInfo memInfo = Ort::MemoryInfo::CreateCpu(
        OrtArenaAllocator,
        OrtMemTypeDefault
    );

    const int64_t featShape[3] = {1, mel.numFrames, mel.nMels};
    const int64_t maskShape[2] = {1, mel.numFrames};

    std::vector<Ort::Value> ctcInputs;
    ctcInputs.reserve(2);
    ctcInputs.push_back(Ort::Value::CreateTensor<float>(
        memInfo,
        mel.features.data(),
        mel.features.size(),
        featShape,
        3
    ));
    if (m_ctcMaskIsBool)
    {
        ctcInputs.push_back(Ort::Value::CreateTensor<bool>(
            memInfo,
            reinterpret_cast<bool *>(mel.attentionMaskBool.data()),
            mel.attentionMaskBool.size(),
            maskShape,
            2
        ));
    }
    else
    {
        ctcInputs.push_back(Ort::Value::CreateTensor<int64_t>(
            memInfo,
            mel.attentionMaskInt64.data(),
            mel.attentionMaskInt64.size(),
            maskShape,
            2
        ));
    }

    const char *inNames[2] = {"input_features", "attention_mask"};
    Ort::AllocatorWithDefaultOptions allocator;
    const size_t outCount = m_ctcSession->GetOutputCount();
    std::vector<std::string> outNameStrs;
    std::vector<const char *> outNames;
    for (size_t i = 0; i < outCount; ++i)
    {
        outNameStrs.emplace_back(m_ctcSession->GetOutputNameAllocated(i, allocator).get());
    }
    for (const std::string &s : outNameStrs)
    {
        outNames.push_back(s.c_str());
    }

    std::vector<Ort::Value> ctcOutputs = m_ctcSession->Run(
        Ort::RunOptions{nullptr},
        inNames,
        ctcInputs.data(),
        ctcInputs.size(),
        outNames.data(),
        outNames.size()
    );

    if (options.abort && options.abort->load())
    {
        return AsrTranscriptionResult::Canceled;
    }

    const Ort::Value &logitsVal = ctcOutputs[0];
    const auto logitsShape = logitsVal.GetTensorTypeAndShapeInfo().GetShape();
    const int64_t outFrames = logitsShape.at(1);
    const int64_t vocabDim = logitsShape.at(2);
    const int64_t blankId = (m_config.ctcBlankTokenId >= 0 && m_config.ctcBlankTokenId < vocabDim)
        ? m_config.ctcBlankTokenId
        : (vocabDim - 1);

    const float *logitsData = logitsVal.GetTensorData<float>();
    const int64_t *outMaskData = nullptr;
    if (ctcOutputs.size() > 1)
    {
        const auto maskInfo = ctcOutputs[1].GetTensorTypeAndShapeInfo();
        if (maskInfo.GetElementType() == ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64)
        {
            outMaskData = ctcOutputs[1].GetTensorData<int64_t>();
        }
    }

    // Duration of one subsampled CTC output frame (8 * 160 / 16000 = 0.08 s)
    const double frameSec =
        static_cast<double>(m_config.subsamplingFactor * m_config.frontend.hopLength) /
        static_cast<double>(m_config.frontend.sampleRate);
    const double totalClipSec =
        static_cast<double>(samples.size()) / static_cast<double>(m_config.frontend.sampleRate);

    // Greedy CTC decoding with frame-level timestamps
    constexpr int64_t SILENCE_SPLIT_FRAMES = 5; // 5 * 80 ms = 400 ms pause splits utterance
    std::vector<int64_t> currentTokens;
    int64_t cueStartFrame = -1;
    int64_t lastNonBlankFrame = -1;
    int64_t prevToken = -1;

    const auto flushCue = [&]()
    {
        if (currentTokens.empty() || cueStartFrame < 0 || lastNonBlankFrame < cueStartFrame)
        {
            currentTokens.clear();
            cueStartFrame = -1;
            return;
        }
        const QString text = m_tokenizer.decode(currentTokens, true).trimmed();
        if (!text.isEmpty() && options.segmentCallback)
        {
            const double startSec = std::max(0.0, static_cast<double>(cueStartFrame) * frameSec - 0.04);
            const double endSec = std::min(
                totalClipSec,
                std::max(startSec + 0.16, static_cast<double>(lastNonBlankFrame + 1) * frameSec + 0.08)
            );
            options.segmentCallback(SubtitleEntry{
                .text = text,
                .start = startSec,
                .end = endSec,
            });
        }
        currentTokens.clear();
        cueStartFrame = -1;
    };

    for (int64_t t = 0; t < outFrames; ++t)
    {
        int64_t bestId = blankId;
        if (outMaskData == nullptr || outMaskData[t] != 0)
        {
            const float *row = logitsData + t * vocabDim;
            bestId = 0;
            float maxVal = row[0];
            for (int64_t v = 1; v < vocabDim; ++v)
            {
                if (row[v] > maxVal)
                {
                    maxVal = row[v];
                    bestId = v;
                }
            }
        }

        if (bestId == blankId)
        {
            if (cueStartFrame >= 0 && (t - lastNonBlankFrame) >= SILENCE_SPLIT_FRAMES)
            {
                flushCue();
            }
            prevToken = blankId;
            continue;
        }

        if (bestId != prevToken)
        {
            if (cueStartFrame < 0)
            {
                cueStartFrame = t;
            }
            currentTokens.push_back(bestId);
            lastNonBlankFrame = t;

            const QString tokStr = m_tokenizer.idToToken(bestId);
            const double curDuration = static_cast<double>(t - cueStartFrame + 1) * frameSec;
            if (curDuration >= 1.0 && endsWithJapaneseSentencePunctuation(tokStr))
            {
                flushCue();
            }
        }
        else
        {
            lastNonBlankFrame = t;
        }
        prevToken = bestId;
    }

    flushCue();
    return AsrTranscriptionResult::Success;
}

#endif // HAVE_ASR_BACKEND_ONNX
