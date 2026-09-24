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
#include "manager/downloadmanager.h"
#include "state/context.h"
#include "setting/settings.h"
#include "util/directoryutils.h"
#include <QDir>
#include <QFileInfo>
#include <QPointer>
#include <utility>

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
    if (m_context == nullptr || m_context->settings() == nullptr)
    {
        return;
    }

    Settings *settings = m_context->settings();
    const auto notifyConfigurationChanged = [this]
    {
        emit configurationChanged();
    };
    connect(
        settings,
        &Settings::whisperEnabledChanged,
        this,
        notifyConfigurationChanged
    );
    connect(
        settings,
        &Settings::whisperModelChanged,
        this,
        notifyConfigurationChanged
    );
    connect(
        settings,
        &Settings::whisperCustomModelChanged,
        this,
        notifyConfigurationChanged
    );
    connect(
        settings,
        &Settings::whisperVadEnabledChanged,
        this,
        notifyConfigurationChanged
    );
    connect(
        settings,
        &Settings::whisperVadModelChanged,
        this,
        notifyConfigurationChanged
    );
    connect(
        settings,
        &Settings::whisperUseGpuChanged,
        this,
        notifyConfigurationChanged
    );
    connect(
        settings,
        &Settings::whisperGpuDeviceChanged,
        this,
        notifyConfigurationChanged
    );
    connect(
        settings,
        &Settings::whisperThreadsChanged,
        this,
        notifyConfigurationChanged
    );
    connect(
        settings,
        &Settings::whisperBestOfChanged,
        this,
        notifyConfigurationChanged
    );
    connect(
        settings,
        &Settings::whisperBeamSizeChanged,
        this,
        notifyConfigurationChanged
    );
    connect(
        settings,
        &Settings::whisperFlashAttentionChanged,
        this,
        notifyConfigurationChanged
    );
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

bool WhisperController::hasAnyModel() const
{
    QDir dir(modelsDirectory());
    const QFileInfoList entries = dir.entryInfoList(
        QStringList{"*"},
        QDir::Files | QDir::Readable
    );
    for (const QFileInfo &entry : entries)
    {
        const QString filename = entry.fileName();
        if (!filename.endsWith(MODEL_SUFFIX) ||
            filename == VAD_SILERO_5 ||
            filename == VAD_SILERO_6)
        {
            continue;
        }
        return true;
    }
    return false;
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

QCoro::Task<WhisperBackendCreationResult> WhisperController::createBackend(
    QString modelPath)
{
#ifdef MEMENTO_WHISPER_SUPPORT
    WhisperBackendCreationResult result;
    Settings *settings = m_context == nullptr ? nullptr : m_context->settings();
    if (settings == nullptr)
    {
        result.error = tr("Whisper settings are not available.");
        co_return std::move(result);
    }

    WhisperBackendOptions options;
    options.modelPath = std::move(modelPath);
    options.useGpu = settings->whisperUseGpu();
    options.gpuDevice = settings->whisperGpuDevice();
    options.flashAttention = settings->whisperFlashAttention();
    options.threads = settings->whisperThreads();
    options.bestOf = settings->whisperBestOf();
    options.beamSize = settings->whisperBeamSize();
    options.useVad = settings->whisperVadEnabled();

    if (options.useVad)
    {
        QPointer<WhisperController> self(this);
        options.vadModel = resolveModelPath(
            settings->whisperVadModel(),
            QStringList{MODEL_SUFFIX}
        );
        const QUrl url = vadModelUrl(settings->whisperVadModel());
        if (!url.isEmpty() && !QFileInfo::exists(options.vadModel))
        {
            DownloadManager *downloader = m_context->downloadManager();
            if (downloader == nullptr)
            {
                result.error = tr("Could not download Whisper VAD model: %1")
                    .arg(options.vadModel);
                co_return std::move(result);
            }

            const bool downloaded = co_await downloader->download(
                url,
                options.vadModel,
                settings->whisperVadModel()
            );
            if (self == nullptr)
            {
                co_return std::move(result);
            }
            if (!downloaded)
            {
                result.error = tr("Could not download Whisper VAD model: %1")
                    .arg(options.vadModel);
                co_return std::move(result);
            }
        }
        if (options.vadModel.isEmpty() ||
            !QFileInfo::exists(options.vadModel))
        {
            result.error = tr("Whisper VAD model was not found: %1")
                .arg(options.vadModel);
            co_return std::move(result);
        }
    }

    result.backend = std::make_unique<WhisperBackend>(std::move(options));
    co_return std::move(result);
#else
    Q_UNUSED(modelPath)
    WhisperBackendCreationResult result;
    result.error = tr(
        "Whisper subtitle support is not available in this build.");
    co_return std::move(result);
#endif
}
