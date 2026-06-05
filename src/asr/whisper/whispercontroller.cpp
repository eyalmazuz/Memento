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

#include "asr/whisper/whispercontroller.h"
#include "state/context.h"
#include "setting/settings.h"
#include "util/directoryutils.h"
#include <QDir>
#include <QFileInfo>

#ifdef MEMENTO_WHISPER_SUPPORT
#include "asr/whisper/whisperbackend.h"
#endif

static constexpr const char *MODEL_PREFIX = "ggml-";
static constexpr const char *MODEL_SUFFIX = ".bin";
static constexpr const char *MODEL_CUSTOM = "custom";
static constexpr const char *WHISPER_MODEL_DIR = "models";

static constexpr const char *WHISPER_MODEL_URL =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";
static constexpr const char *WHISPER_VAD_URL =
    "https://huggingface.co/ggml-org/whisper-vad/resolve/main/";

static constexpr const char *VAD_SILERO_5 = "ggml-silero-v5.1.2.bin";
static constexpr const char *VAD_SILERO_6 = "ggml-silero-v6.2.0.bin";

WhisperController::WhisperController(Context *context, QObject *parent) :
    QObject(parent),
    m_context(context)
{
}

WhisperController::~WhisperController() = default;

bool WhisperController::isManagedModel(const QString &model) const
{
    static const QStringList MODELS{
        "tiny",
        "base",
        "small",
        "medium",
        "large-v3",
        "large-v3-turbo",
    };
    return MODELS.contains(model);
}

QString WhisperController::modelFilename(const QString &model) const
{
    return QString("%1%2%3").arg(MODEL_PREFIX, model, MODEL_SUFFIX);
}

QUrl WhisperController::whisperModelUrl(const QString &model) const
{
    return QUrl(QString("%1%2").arg(WHISPER_MODEL_URL, modelFilename(model)));
}

QUrl WhisperController::vadModelUrl(const QString &modelPath) const
{
    const QFileInfo info(modelPath);
    if (info.isAbsolute())
    {
        return {};
    }

    const QString filename = info.fileName();
    if (filename != VAD_SILERO_5 && filename != VAD_SILERO_6)
    {
        return {};
    }

    return QUrl(QString("%1%2").arg(WHISPER_VAD_URL, filename));
}

bool WhisperController::modelAvailable(const QString &model) const
{
    if (!isManagedModel(model))
    {
        return false;
    }
    return QFileInfo::exists(
        resolveModelPath(modelFilename(model), QStringList{MODEL_SUFFIX})
    );
}

QString WhisperController::modelsDirectory() const
{
    return QDir(DirectoryUtils::getConfigDir()).filePath(WHISPER_MODEL_DIR);
}

QString WhisperController::resolveModelPath(
    const QString &path,
    const QStringList &suffixes) const
{
    if (path.isEmpty())
    {
        return {};
    }

    const QFileInfo info(path);
    if (info.isAbsolute())
    {
        return path;
    }

    const QDir modelDir(modelsDirectory());
    QStringList filenames{path};
    for (const QString &suffix : suffixes)
    {
        if (!path.endsWith(suffix))
        {
            filenames.append(path + suffix);
        }
    }
    filenames.removeDuplicates();

    for (const QString &filename : filenames)
    {
        const QString candidate = modelDir.filePath(filename);
        if (QFileInfo::exists(candidate))
        {
            return candidate;
        }
    }

    const bool bareFilename = info.fileName() == path;
    if (bareFilename)
    {
        return modelDir.filePath(filenames.last());
    }

    return info.absoluteFilePath();
}

QString WhisperController::selectedModelPath() const
{
    if (m_context == nullptr || m_context->settings() == nullptr)
    {
        return {};
    }

    Settings *settings = m_context->settings();
    const QString model = settings->whisperModel();
    if (model == MODEL_CUSTOM)
    {
        return resolveModelPath(
            settings->whisperCustomModel(),
            QStringList{MODEL_SUFFIX}
        );
    }

    return resolveModelPath(modelFilename(model), QStringList{MODEL_SUFFIX});
}

std::unique_ptr<AsrBackend> WhisperController::createBackend(
    const QString &modelPath,
    bool useGpu,
    int gpuDevice,
    bool flashAttention)
{
#ifdef MEMENTO_WHISPER_SUPPORT
    return std::make_unique<WhisperBackend>(
        modelPath,
        useGpu,
        gpuDevice,
        flashAttention
    );
#else
    Q_UNUSED(modelPath)
    Q_UNUSED(useGpu)
    Q_UNUSED(gpuDevice)
    Q_UNUSED(flashAttention)
    return nullptr;
#endif
}
