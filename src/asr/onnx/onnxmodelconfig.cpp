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

#include "asr/onnx/onnxmodelconfig.h"

#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>

namespace
{

QJsonObject readJsonObject(const QString &path)
{
    QFile file(path);
    if (!file.open(QFile::ReadOnly))
    {
        return {};
    }
    QJsonParseError parseError;
    const QJsonDocument doc = QJsonDocument::fromJson(file.readAll(), &parseError);
    if (parseError.error != QJsonParseError::NoError || !doc.isObject())
    {
        return {};
    }
    return doc.object();
}

QString findFirstExistingFile(const QDir &dir, const QStringList &relativeCandidates)
{
    for (const QString &rel : relativeCandidates)
    {
        const QString fullPath = dir.filePath(rel);
        if (QFileInfo::exists(fullPath) && QFileInfo(fullPath).isFile())
        {
            return fullPath;
        }
    }
    return {};
}

} // namespace

OnnxModelConfig OnnxModelConfig::loadFromPath(const QString &path, QString &error)
{
    OnnxModelConfig cfg;
    if (path.isEmpty())
    {
        error = QStringLiteral("ONNX model path is empty.");
        return cfg;
    }

    QFileInfo info(path);
    if (!info.exists())
    {
        error = QStringLiteral("ONNX model path does not exist: %1").arg(path);
        return cfg;
    }

    QDir dir(info.isDir() ? info.absoluteFilePath() : info.absolutePath());
    // If pointed inside an "onnx" subdirectory, check parent directory for config/tokenizer
    if (!QFileInfo::exists(dir.filePath(QStringLiteral("tokenizer.json"))) &&
        dir.dirName() == QStringLiteral("onnx"))
    {
        QDir parentDir = dir;
        if (parentDir.cdUp() &&
            QFileInfo::exists(parentDir.filePath(QStringLiteral("tokenizer.json"))))
        {
            dir = parentDir;
        }
    }
    cfg.modelDir = dir.absolutePath();

    cfg.tokenizerJsonPath = findFirstExistingFile(
        dir,
        {
            QStringLiteral("tokenizer.json"),
            QStringLiteral("onnx/tokenizer.json"),
        }
    );
    if (cfg.tokenizerJsonPath.isEmpty())
    {
        error = QStringLiteral("tokenizer.json was not found in %1").arg(cfg.modelDir);
        return cfg;
    }

    // Locate ONNX graph files (preferring _int8 single-file models when both exist, or user-selected file)
    cfg.encoderOnnxPath = findFirstExistingFile(
        dir,
        {
            QStringLiteral("encoder_model_int8.onnx"),
            QStringLiteral("onnx/encoder_model_int8.onnx"),
            QStringLiteral("encoder_model_quantized.onnx"),
            QStringLiteral("onnx/encoder_model_quantized.onnx"),
            QStringLiteral("encoder_model.onnx"),
            QStringLiteral("onnx/encoder_model.onnx"),
        }
    );
    cfg.decoderOnnxPath = findFirstExistingFile(
        dir,
        {
            QStringLiteral("decoder_model_int8.onnx"),
            QStringLiteral("onnx/decoder_model_int8.onnx"),
            QStringLiteral("decoder_model_merged_int8.onnx"),
            QStringLiteral("onnx/decoder_model_merged_int8.onnx"),
            QStringLiteral("decoder_model_merged_quantized.onnx"),
            QStringLiteral("onnx/decoder_model_merged_quantized.onnx"),
            QStringLiteral("decoder_model_quantized.onnx"),
            QStringLiteral("onnx/decoder_model_quantized.onnx"),
            QStringLiteral("decoder_model_merged.onnx"),
            QStringLiteral("onnx/decoder_model_merged.onnx"),
            QStringLiteral("decoder_model.onnx"),
            QStringLiteral("onnx/decoder_model.onnx"),
        }
    );
    cfg.ctcOnnxPath = findFirstExistingFile(
        dir,
        {
            QStringLiteral("model_int8.onnx"),
            QStringLiteral("onnx/model_int8.onnx"),
            QStringLiteral("model_quantized.onnx"),
            QStringLiteral("onnx/model_quantized.onnx"),
            QStringLiteral("model.onnx"),
            QStringLiteral("onnx/model.onnx"),
        }
    );

    if (info.isFile() && info.suffix().compare(QStringLiteral("onnx"), Qt::CaseInsensitive) == 0)
    {
        const QString fileName = info.fileName();
        if (!fileName.startsWith(QStringLiteral("encoder_")) &&
            !fileName.startsWith(QStringLiteral("decoder_")))
        {
            cfg.ctcOnnxPath = info.absoluteFilePath();
        }
    }

    // Parse config.json
    const QString configJsonPath = findFirstExistingFile(
        dir,
        {
            QStringLiteral("config.json"),
            QStringLiteral("onnx/config.json"),
        }
    );
    const QJsonObject rootConfig = configJsonPath.isEmpty()
        ? QJsonObject{}
        : readJsonObject(configJsonPath);

    cfg.modelType = rootConfig.value(QStringLiteral("model_type")).toString();
    if (rootConfig.contains(QStringLiteral("subsampling_factor")))
    {
        cfg.subsamplingFactor = rootConfig.value(QStringLiteral("subsampling_factor")).toInt(8);
    }
    if (rootConfig.contains(QStringLiteral("vocab_size")))
    {
        cfg.vocabSize = rootConfig.value(QStringLiteral("vocab_size")).toInteger(0);
    }
    if (rootConfig.contains(QStringLiteral("eos_token_id")))
    {
        cfg.eosTokenId = rootConfig.value(QStringLiteral("eos_token_id")).toInteger(3);
    }
    if (rootConfig.contains(QStringLiteral("pad_token_id")))
    {
        cfg.padTokenId = rootConfig.value(QStringLiteral("pad_token_id")).toInteger(2);
    }
    if (rootConfig.contains(QStringLiteral("bos_token_id")))
    {
        cfg.bosTokenId = rootConfig.value(QStringLiteral("bos_token_id")).toInteger(4);
    }

    if (rootConfig.value(QStringLiteral("encoder_config")).isObject())
    {
        const QJsonObject encObj = rootConfig.value(QStringLiteral("encoder_config")).toObject();
        if (encObj.contains(QStringLiteral("subsampling_factor")))
        {
            cfg.subsamplingFactor = encObj.value(QStringLiteral("subsampling_factor")).toInt(cfg.subsamplingFactor);
        }
        if (encObj.contains(QStringLiteral("num_mel_bins")))
        {
            cfg.frontend.nMels = encObj.value(QStringLiteral("num_mel_bins")).toInt(cfg.frontend.nMels);
        }
    }
    if (rootConfig.value(QStringLiteral("decoder_config")).isObject())
    {
        const QJsonObject decObj = rootConfig.value(QStringLiteral("decoder_config")).toObject();
        if (decObj.contains(QStringLiteral("vocab_size")))
        {
            cfg.vocabSize = decObj.value(QStringLiteral("vocab_size")).toInteger(cfg.vocabSize);
        }
    }

    // Parse generation_config.json if available
    const QString genConfigPath = findFirstExistingFile(
        dir,
        {
            QStringLiteral("generation_config.json"),
            QStringLiteral("onnx/generation_config.json"),
        }
    );
    if (!genConfigPath.isEmpty())
    {
        const QJsonObject genObj = readJsonObject(genConfigPath);
        if (genObj.contains(QStringLiteral("eos_token_id")))
        {
            cfg.eosTokenId = genObj.value(QStringLiteral("eos_token_id")).toInteger(cfg.eosTokenId);
        }
        if (genObj.contains(QStringLiteral("pad_token_id")))
        {
            cfg.padTokenId = genObj.value(QStringLiteral("pad_token_id")).toInteger(cfg.padTokenId);
        }
        if (genObj.contains(QStringLiteral("bos_token_id")))
        {
            cfg.bosTokenId = genObj.value(QStringLiteral("bos_token_id")).toInteger(cfg.bosTokenId);
        }
    }

    // Parse processor_config.json or preprocessor_config.json
    const QString procConfigPath = findFirstExistingFile(
        dir,
        {
            QStringLiteral("processor_config.json"),
            QStringLiteral("preprocessor_config.json"),
            QStringLiteral("onnx/processor_config.json"),
            QStringLiteral("onnx/preprocessor_config.json"),
        }
    );
    if (!procConfigPath.isEmpty())
    {
        const QJsonObject procRoot = readJsonObject(procConfigPath);
        const QJsonObject featObj = procRoot.value(QStringLiteral("feature_extractor")).isObject()
            ? procRoot.value(QStringLiteral("feature_extractor")).toObject()
            : procRoot;

        if (featObj.contains(QStringLiteral("sampling_rate")))
        {
            cfg.frontend.sampleRate = featObj.value(QStringLiteral("sampling_rate")).toInt(16000);
        }
        if (featObj.contains(QStringLiteral("n_fft")))
        {
            cfg.frontend.nFft = featObj.value(QStringLiteral("n_fft")).toInt(512);
        }
        if (featObj.contains(QStringLiteral("win_length")))
        {
            cfg.frontend.winLength = featObj.value(QStringLiteral("win_length")).toInt(400);
        }
        else if (featObj.contains(QStringLiteral("n_window_size")))
        {
            cfg.frontend.winLength = featObj.value(QStringLiteral("n_window_size")).toInt(400);
        }
        if (featObj.contains(QStringLiteral("hop_length")))
        {
            cfg.frontend.hopLength = featObj.value(QStringLiteral("hop_length")).toInt(160);
        }
        else if (featObj.contains(QStringLiteral("n_window_stride")))
        {
            cfg.frontend.hopLength = featObj.value(QStringLiteral("n_window_stride")).toInt(160);
        }
        if (featObj.contains(QStringLiteral("feature_size")))
        {
            cfg.frontend.nMels = featObj.value(QStringLiteral("feature_size")).toInt(cfg.frontend.nMels);
        }
        else if (featObj.contains(QStringLiteral("num_mel_bins")))
        {
            cfg.frontend.nMels = featObj.value(QStringLiteral("num_mel_bins")).toInt(cfg.frontend.nMels);
        }
        if (featObj.contains(QStringLiteral("preemphasis")))
        {
            cfg.frontend.preemphasis = static_cast<float>(
                featObj.value(QStringLiteral("preemphasis")).toDouble(0.97)
            );
        }
        if (featObj.contains(QStringLiteral("max_audio_clip_s")))
        {
            cfg.maxAudioClipSeconds = featObj.value(QStringLiteral("max_audio_clip_s")).toDouble(35.0);
        }
        if (featObj.contains(QStringLiteral("normalize")))
        {
            const QString normMode = featObj.value(QStringLiteral("normalize")).toString();
            cfg.frontend.normalizePerFeature = (normMode == QStringLiteral("per_feature"));
        }
    }

    // Determine model architecture
    if (!cfg.encoderOnnxPath.isEmpty() && !cfg.decoderOnnxPath.isEmpty())
    {
        cfg.architecture = OnnxAsrArchitecture::EncoderDecoder;
    }
    else if (!cfg.ctcOnnxPath.isEmpty())
    {
        cfg.architecture = OnnxAsrArchitecture::Ctc;
        if (cfg.ctcBlankTokenId < 0)
        {
            cfg.ctcBlankTokenId = cfg.padTokenId > 0 ? cfg.padTokenId : cfg.vocabSize;
        }
        if (cfg.modelType == QStringLiteral("parakeet_ctc") && cfg.frontend.nMels == 128 &&
            !rootConfig.contains(QStringLiteral("encoder_config")))
        {
            cfg.frontend.nMels = rootConfig.value(QStringLiteral("num_mel_bins")).toInt(80);
        }
    }
    else
    {
        error = QStringLiteral(
            "No ONNX model files (encoder_model*.onnx + decoder_model*.onnx or model*.onnx) found in %1"
        ).arg(cfg.modelDir);
    }

    return cfg;
}
