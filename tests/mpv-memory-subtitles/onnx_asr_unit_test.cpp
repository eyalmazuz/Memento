#include <cmath>
#include <iostream>
#include <vector>

#include <QCoreApplication>
#include <QDir>
#include <QFile>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QTemporaryDir>

#include "asr/audio/melfrontend.h"
#include "asr/audio/wavreader.h"
#include "asr/onnx/onnxmodelconfig.h"
#include "asr/onnx/onnxtokenizer.h"

namespace
{

bool check(const bool condition, const char *message)
{
    if (!condition)
    {
        std::cerr << "FAIL: " << message << std::endl;
        return false;
    }
    return true;
}

bool writeTextFile(const QString &path, const QByteArray &data)
{
    QFile f(path);
    if (!f.open(QIODevice::WriteOnly))
    {
        return false;
    }
    f.write(data);
    return true;
}

bool testMelFrontend()
{
    // 1.5 seconds of 440 Hz sine wave at 16 kHz = 24,000 samples
    constexpr size_t numSamples = 24000;
    std::vector<float> samples(numSamples, 0.0f);
    for (size_t i = 0; i < numSamples; ++i)
    {
        samples[i] = 0.5f * std::sin(2.0f * static_cast<float>(M_PI) * 440.0f *
                                     static_cast<float>(i) / 16000.0f);
    }

    // Test 128-mel (CohereAsrFeatureExtractor)
    AsrAudio::MelFrontendConfig cfg128{};
    cfg128.nMels = 128;
    const AsrAudio::MelFrontend fe128(cfg128);
    const AsrAudio::MelFeatureOutput out128 = fe128.extract(samples);
    if (!check(out128.nMels == 128, "128-mel nMels mismatch"))
    {
        return false;
    }
    if (!check(out128.validFrames == 150, "128-mel validFrames should be 24000 / 160 = 150"))
    {
        return false;
    }
    if (!check(out128.features.size() ==
                   static_cast<size_t>(out128.numFrames) * static_cast<size_t>(out128.nMels),
               "128-mel data size mismatch"))
    {
        return false;
    }

    // Check per-feature zero-mean, unit-variance normalization across validFrames
    for (int m = 0; m < 128; ++m)
    {
        double sum = 0.0;
        for (int64_t t = 0; t < out128.validFrames; ++t)
        {
            sum += out128.features[static_cast<size_t>(t) * 128 + static_cast<size_t>(m)];
        }
        const double mean = sum / static_cast<double>(out128.validFrames);
        if (!check(std::abs(mean) < 1e-4, "128-mel per-feature mean is not ~0"))
        {
            return false;
        }
    }

    // Test 80-mel (ParakeetFeatureExtractor)
    AsrAudio::MelFrontendConfig cfg80{};
    cfg80.nMels = 80;
    const AsrAudio::MelFrontend fe80(cfg80);
    const AsrAudio::MelFeatureOutput out80 = fe80.extract(samples);
    if (!check(out80.nMels == 80 && out80.validFrames == 150, "80-mel dimensions mismatch"))
    {
        return false;
    }

    // Test utterance segmentation with two bursts separated by silence
    std::vector<float> twoBursts(16000 * 4, 0.0f);
    for (size_t i = 1600; i < 16000; ++i)
    {
        twoBursts[i] = 0.4f * std::sin(2.0f * static_cast<float>(M_PI) * 300.0f *
                                       static_cast<float>(i) / 16000.0f);
    }
    for (size_t i = 32000; i < 48000; ++i)
    {
        twoBursts[i] = 0.4f * std::sin(2.0f * static_cast<float>(M_PI) * 300.0f *
                                       static_cast<float>(i) / 16000.0f);
    }
    const auto spans = AsrAudio::MelFrontend::segmentSpeechUtterances(
        twoBursts.data(), twoBursts.size(), 16000);
    if (!check(spans.size() == 2, "segmentSpeechUtterances should detect 2 speech spans"))
    {
        return false;
    }

    return true;
}

bool testOnnxTokenizerAndConfig()
{
    QTemporaryDir tempDir;
    if (!check(tempDir.isValid(), "Failed to create temporary directory"))
    {
        return false;
    }

    // 1. Write synthetic tokenizer.json with BPE object vocab, added_tokens, and byte fallback
    {
        QJsonObject vocab;
        vocab.insert(QStringLiteral("<pad>"), 0);
        vocab.insert(QStringLiteral("<eos>"), 1);
        vocab.insert(QStringLiteral("<unk>"), 2);
        vocab.insert(QString::fromUtf8("\xE2\x96\x81"), 13764);                       // ▁ (bos_token_id)
        vocab.insert(QString::fromUtf8("\xE2\x96\x81\xE6\x97\xA5\xE6\x9C\xAC"), 100); // ▁日本
        vocab.insert(QString::fromUtf8("\xE8\xAA\x9E"), 101);                         // 語
        vocab.insert(QStringLiteral("<0xE3>"), 200);
        vocab.insert(QStringLiteral("<0x81>"), 201);
        vocab.insert(QStringLiteral("<0x82>"), 202); // UTF-8 for 'あ'

        QJsonObject model;
        model.insert(QStringLiteral("vocab"), vocab);

        QJsonArray addedTokens;
        auto addSpecial = [&](int id, const QString &content) {
            QJsonObject obj;
            obj.insert(QStringLiteral("id"), id);
            obj.insert(QStringLiteral("content"), content);
            obj.insert(QStringLiteral("special"), true);
            addedTokens.append(obj);
        };
        addSpecial(4, QStringLiteral("<|startoftranscript|>"));
        addSpecial(5, QStringLiteral("<|pnc|>"));
        addSpecial(6, QStringLiteral("<|nopnc|>"));
        addSpecial(7, QStringLiteral("<|startofcontext|>"));
        addSpecial(9, QStringLiteral("<|noitn|>"));
        addSpecial(11, QStringLiteral("<|notimestamp|>"));
        addSpecial(13, QStringLiteral("<|nodiarize|>"));
        addSpecial(16, QStringLiteral("<|emo:undefined|>"));
        addSpecial(98, QStringLiteral("<|ja|>"));

        QJsonObject root;
        root.insert(QStringLiteral("model"), model);
        root.insert(QStringLiteral("added_tokens"), addedTokens);

        if (!check(writeTextFile(QDir(tempDir.path()).filePath(QStringLiteral("tokenizer.json")),
                                 QJsonDocument(root).toJson()),
                   "Failed to write tokenizer.json"))
        {
            return false;
        }
    }

    OnnxTokenizer tokenizer;
    QString err;
    if (!check(tokenizer.load(QDir(tempDir.path()).filePath(QStringLiteral("tokenizer.json")), err),
               "OnnxTokenizer::load failed"))
    {
        return false;
    }

    const QString decoded = tokenizer.decode({4, 100, 101, 200, 201, 202, 1}, true);
    if (!check(decoded == QStringLiteral("日本語あ"), "OnnxTokenizer::decode UTF-8 / metaspace mismatch"))
    {
        std::cerr << "Decoded got: " << decoded.toStdString() << std::endl;
        return false;
    }

    const std::vector<int64_t> prompt = tokenizer.buildCohereAsrPrompt(QStringLiteral("ja"), true);
    const std::vector<int64_t> expectedPrompt = {13764, 7, 4, 16, 98, 98, 5, 9, 11, 13};
    if (!check(prompt == expectedPrompt, "Cohere 10-token decoder prompt mismatch"))
    {
        return false;
    }

    // 2. Test EncoderDecoder config detection (kitsune-transcribe-t0.6b-ja layout)
    {
        if (!check(writeTextFile(QDir(tempDir.path()).filePath(QStringLiteral("config.json")),
                                 R"({
            "model_type": "cohere_asr",
            "bos_token_id": 13764,
            "eos_token_id": 1,
            "pad_token_id": 0,
            "max_target_positions": 1024,
            "encoder_config": { "subsampling_factor": 8 }
        })"),
                   "Failed to write cohere_asr config.json"))
        {
            return false;
        }

        if (!check(writeTextFile(QDir(tempDir.path()).filePath(QStringLiteral("processor_config.json")),
                                 R"({
            "feature_extractor": {
                "feature_size": 128,
                "sampling_rate": 16000,
                "hop_length": 160,
                "n_fft": 512,
                "win_length": 400,
                "preemphasis": 0.97
            }
        })"),
                   "Failed to write cohere_asr processor_config.json"))
        {
            return false;
        }

        QDir(tempDir.path()).mkdir(QStringLiteral("onnx"));
        if (!check(writeTextFile(
                       QDir(tempDir.path()).filePath(QStringLiteral("onnx/encoder_model_quantized.onnx")),
                       "dummy"),
                   "Failed to write encoder_model_quantized.onnx") ||
            !check(writeTextFile(
                       QDir(tempDir.path()).filePath(QStringLiteral("onnx/decoder_model_merged_quantized.onnx")),
                       "dummy"),
                   "Failed to write decoder_model_merged_quantized.onnx"))
        {
            return false;
        }

        const OnnxModelConfig modelCfg = OnnxModelConfig::loadFromPath(tempDir.path(), err);
        if (!check(modelCfg.isValid(), "OnnxModelConfig::loadFromPath (cohere_asr) failed"))
        {
            std::cerr << "Error: " << err.toStdString() << std::endl;
            return false;
        }
        if (!check(modelCfg.architecture == OnnxAsrArchitecture::EncoderDecoder,
                   "Expected EncoderDecoder architecture"))
        {
            return false;
        }
        if (!check(modelCfg.frontend.nMels == 128, "Expected 128 mel bins"))
        {
            return false;
        }
        if (!check(modelCfg.bosTokenId == 13764 && modelCfg.eosTokenId == 1,
                   "BOS/EOS token IDs mismatch"))
        {
            return false;
        }
    }

    // 3. Test Parakeet CTC config detection (kitsune-transcribe-p0.3b-ja layout)
    QTemporaryDir ctcDir;
    {
        if (!check(writeTextFile(QDir(ctcDir.path()).filePath(QStringLiteral("tokenizer.json")),
                                 R"({"model":{"vocab":{"<unk>":0,"a":1,"<pad>":3072}}})"),
                   "Failed to write ctc tokenizer.json") ||
            !check(writeTextFile(QDir(ctcDir.path()).filePath(QStringLiteral("config.json")),
                                 R"({
            "model_type": "parakeet_ctc",
            "pad_token_id": 3072,
            "vocab_size": 3073,
            "encoder_config": { "subsampling_factor": 8 }
        })"),
                   "Failed to write ctc config.json") ||
            !check(writeTextFile(QDir(ctcDir.path()).filePath(QStringLiteral("processor_config.json")),
                                 R"({
            "feature_extractor": {
                "feature_size": 80,
                "sampling_rate": 16000
            }
        })"),
                   "Failed to write ctc processor_config.json"))
        {
            return false;
        }

        QDir(ctcDir.path()).mkdir(QStringLiteral("onnx"));
        if (!check(writeTextFile(QDir(ctcDir.path()).filePath(QStringLiteral("onnx/model_quantized.onnx")),
                                 "dummy"),
                   "Failed to write ctc model_quantized.onnx"))
        {
            return false;
        }

        const OnnxModelConfig ctcCfg = OnnxModelConfig::loadFromPath(ctcDir.path(), err);
        if (!check(ctcCfg.isValid(), "OnnxModelConfig::loadFromPath (parakeet_ctc) failed"))
        {
            std::cerr << "Error: " << err.toStdString() << std::endl;
            return false;
        }
        if (!check(ctcCfg.architecture == OnnxAsrArchitecture::Ctc,
                   "Expected Ctc architecture"))
        {
            return false;
        }
        if (!check(ctcCfg.frontend.nMels == 80 && ctcCfg.ctcBlankTokenId == 3072,
                   "Expected 80 mel bins and ctcBlankTokenId 3072"))
        {
            return false;
        }
    }

    return true;
}

} // namespace

#ifdef HAVE_ASR_BACKEND_ONNX
#include <QElapsedTimer>
#include <cstring>
#include "asr/onnx/onnxsession.h"

namespace
{
bool writeTestWav16k(const QString &path, const std::vector<float> &samples)
{
    QFile f(path);
    if (!f.open(QIODevice::WriteOnly | QIODevice::Truncate))
    {
        return false;
    }
    const uint32_t numSamples = static_cast<uint32_t>(samples.size());
    const uint32_t dataBytes = numSamples * sizeof(int16_t);
    const uint32_t riffSize = 36 + dataBytes;
    const uint32_t fmtSize = 16;
    const uint16_t audioFormat = 1;
    const uint16_t numChannels = 1;
    const uint32_t sampleRate = 16000;
    const uint32_t byteRate = sampleRate * sizeof(int16_t);
    const uint16_t blockAlign = sizeof(int16_t);
    const uint16_t bitsPerSample = 16;

    f.write("RIFF", 4);
    f.write(reinterpret_cast<const char *>(&riffSize), 4);
    f.write("WAVE", 4);
    f.write("fmt ", 4);
    f.write(reinterpret_cast<const char *>(&fmtSize), 4);
    f.write(reinterpret_cast<const char *>(&audioFormat), 2);
    f.write(reinterpret_cast<const char *>(&numChannels), 2);
    f.write(reinterpret_cast<const char *>(&sampleRate), 4);
    f.write(reinterpret_cast<const char *>(&byteRate), 4);
    f.write(reinterpret_cast<const char *>(&blockAlign), 2);
    f.write(reinterpret_cast<const char *>(&bitsPerSample), 2);
    f.write("data", 4);
    f.write(reinterpret_cast<const char *>(&dataBytes), 4);

    std::vector<int16_t> pcm(numSamples);
    for (size_t i = 0; i < numSamples; ++i)
    {
        const float clamped = std::max(-1.0f, std::min(1.0f, samples[i]));
        pcm[i] = static_cast<int16_t>(clamped * 32767.0f);
    }
    f.write(reinterpret_cast<const char *>(pcm.data()), dataBytes);
    f.close();
    return true;
}
} // namespace
#endif

int main(int argc, char **argv)
{
    QCoreApplication app(argc, argv);
    if (!testMelFrontend())
    {
        return 1;
    }
    if (!testOnnxTokenizerAndConfig())
    {
        return 1;
    }
#ifdef HAVE_ASR_BACKEND_ONNX
    if (argc > 1)
    {
        QElapsedTimer timer;
        timer.start();
        OnnxSessionOptions opts;
        opts.modelPath = QString::fromLocal8Bit(argv[1]);
        opts.useGpu = false;
        opts.threads = 8;
        opts.punctuation = true;
        OnnxSession session(opts);
        std::cout << "Session constructor elapsed_ms=" << timer.elapsed()
                  << " isLoaded=" << session.isLoaded()
                  << " error=" << session.loadError().toStdString() << std::endl;
        if (!session.isLoaded())
        {
            return 1;
        }

        QTemporaryDir wavDir;
        const QString wavPath = QDir(wavDir.path()).filePath(QStringLiteral("test_clip.wav"));
        std::vector<float> samples(16000 * 2, 0.0f);
        for (size_t i = 2000; i < 24000; ++i)
        {
            samples[i] = 0.2f * std::sin(2.0f * 3.14159265f * 300.0f * static_cast<float>(i) / 16000.0f);
        }
        if (!writeTestWav16k(wavPath, samples))
        {
            std::cerr << "Failed to write test WAV" << std::endl;
            return 1;
        }

        AsrTranscriptionOptions transOpts;
        transOpts.language = QStringLiteral("ja");
        transOpts.segmentCallback = [](SubtitleEntry entry)
        {
            std::cout << "Emitted segment [" << entry.start << " -> " << entry.end
                      << "]: " << entry.text.toStdString() << std::endl;
        };

        const AsrTranscriptionResult res = session.transcribe(wavPath, transOpts).result();
        std::cout << "Transcribe elapsed_ms=" << timer.elapsed()
                  << " result=" << static_cast<int>(res)
                  << " isLoaded=" << session.isLoaded()
                  << " error=" << session.loadError().toStdString() << std::endl;
        if (res != AsrTranscriptionResult::Success)
        {
            return 1;
        }
    }
#endif
    std::cout << "PASS: MelFrontend (80/128-mel), OnnxTokenizer, and OnnxModelConfig (cohere_asr & parakeet_ctc)"
              << std::endl;
    return 0;
}
