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

#include "asr/onnx/onnxcontroller.h"

#include <utility>

#include <QDir>
#include <QFileInfo>
#include <QtConcurrent>

#ifdef MEMENTO_SYSTEM_QCORO
#include <QCoroFuture>
#else
#include <qcoro/core/qcorofuture.h>
#endif // MEMENTO_SYSTEM_QCORO

#include "asr/onnx/onnxmodelconfig.h"
#include "manager/downloadmanager.h"
#include "setting/settings.h"
#include "state/context.h"
#include "util/directoryutils.h"

#ifdef HAVE_ASR_BACKEND_ONNX
#include "asr/onnx/onnxbackend.h"
#endif

namespace
{

constexpr const char *MODEL_CUSTOM = "custom";
constexpr const char *ONNX_MODEL_DIR = "models";

constexpr const char *PRESET_KITSUNE_T06B_JA = "kitsune-transcribe-t0.6b-ja";
constexpr const char *PRESET_KITSUNE_P03B_JA = "kitsune-transcribe-p0.3b-ja";

} // namespace

OnnxController::OnnxController(Context *context, QObject *parent) :
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
        &Settings::onnxModelChanged,
        this,
        notifyConfigurationChanged
    );
    connect(
        settings,
        &Settings::onnxCustomModelChanged,
        this,
        notifyConfigurationChanged
    );
    connect(
        settings,
        &Settings::onnxUseGpuChanged,
        this,
        notifyConfigurationChanged
    );
    connect(
        settings,
        &Settings::onnxGpuDeviceChanged,
        this,
        notifyConfigurationChanged
    );
    connect(
        settings,
        &Settings::onnxThreadsChanged,
        this,
        notifyConfigurationChanged
    );
    connect(
        settings,
        &Settings::onnxPunctuationChanged,
        this,
        notifyConfigurationChanged
    );
}

OnnxController::~OnnxController() = default;

bool OnnxController::isManagedModel(const QString &model) const
{
    return model == QLatin1String(PRESET_KITSUNE_T06B_JA) ||
           model == QLatin1String(PRESET_KITSUNE_P03B_JA);
}

std::vector<OnnxModelFileSpec> OnnxController::modelFiles(const QString &model) const
{
    if (model == QLatin1String(PRESET_KITSUNE_T06B_JA))
    {
        const QString base = QStringLiteral(
            "https://huggingface.co/Multy123/kitsune-transcribe-t0.6b-ja/resolve/main/"
        );
        return {
            {QStringLiteral("config.json"), QUrl(base + QStringLiteral("config.json"))},
            {QStringLiteral("generation_config.json"), QUrl(base + QStringLiteral("generation_config.json"))},
            {QStringLiteral("processor_config.json"), QUrl(base + QStringLiteral("processor_config.json"))},
            {QStringLiteral("tokenizer.json"), QUrl(base + QStringLiteral("tokenizer.json"))},
            {QStringLiteral("decoder_model_int8.onnx"), QUrl(base + QStringLiteral("onnx/decoder_model_int8.onnx"))},
            {QStringLiteral("encoder_model_int8.onnx"), QUrl(base + QStringLiteral("onnx/encoder_model_int8.onnx"))},
        };
    }

    if (model == QLatin1String(PRESET_KITSUNE_P03B_JA))
    {
        const QString base = QStringLiteral(
            "https://huggingface.co/Multy123/kitsune-transcribe-p0.3b-ja/resolve/main/"
        );
        return {
            {QStringLiteral("config.json"), QUrl(base + QStringLiteral("config.json"))},
            {QStringLiteral("processor_config.json"), QUrl(base + QStringLiteral("processor_config.json"))},
            {QStringLiteral("tokenizer.json"), QUrl(base + QStringLiteral("tokenizer.json"))},
            {QStringLiteral("model_int8.onnx"), QUrl(base + QStringLiteral("onnx/model_int8.onnx"))},
        };
    }

    return {};
}

QString OnnxController::modelsDirectory() const
{
    return QDir(DirectoryUtils::getConfigDir()).filePath(ONNX_MODEL_DIR);
}

QString OnnxController::modelDirectory(const QString &model) const
{
    return QDir(modelsDirectory()).filePath(model);
}

bool OnnxController::modelAvailable(const QString &model) const
{
    if (model.isEmpty())
    {
        return false;
    }

    const QString path = isManagedModel(model) ? modelDirectory(model) : model;
    QString err;
    const OnnxModelConfig cfg = OnnxModelConfig::loadFromPath(path, err);
    return cfg.isValid();
}

bool OnnxController::hasAnyModel() const
{
    if (modelAvailable(QString::fromLatin1(PRESET_KITSUNE_T06B_JA)) ||
        modelAvailable(QString::fromLatin1(PRESET_KITSUNE_P03B_JA)))
    {
        return true;
    }

    const QString selected = selectedModelPath();
    if (!selected.isEmpty())
    {
        QString err;
        if (OnnxModelConfig::loadFromPath(selected, err).isValid())
        {
            return true;
        }
    }

    QDir dir(modelsDirectory());
    const QFileInfoList subdirs = dir.entryInfoList(
        QDir::Dirs | QDir::NoDotAndDotDot | QDir::Readable
    );
    for (const QFileInfo &entry : subdirs)
    {
        QString err;
        if (OnnxModelConfig::loadFromPath(entry.absoluteFilePath(), err).isValid())
        {
            return true;
        }
    }
    return false;
}

QString OnnxController::selectedModelPath() const
{
    if (m_context == nullptr || m_context->settings() == nullptr)
    {
        return {};
    }

    Settings *settings = m_context->settings();
    const QString model = settings->onnxModel();
    if (model == QLatin1String(MODEL_CUSTOM))
    {
        const QString custom = settings->onnxCustomModel().trimmed();
        if (custom.isEmpty())
        {
            return {};
        }
        const QFileInfo info(custom);
        if (info.isAbsolute())
        {
            return custom;
        }
        return QDir(modelsDirectory()).filePath(custom);
    }

    return modelDirectory(model);
}

QCoro::Task<bool> OnnxController::downloadManagedModel(const QString &model)
{
    if (m_context == nullptr || m_context->downloadManager() == nullptr)
    {
        co_return false;
    }

    const std::vector<OnnxModelFileSpec> files = modelFiles(model);
    if (files.empty())
    {
        co_return false;
    }

    DownloadManager *downloader = m_context->downloadManager();
    const QDir targetDir(modelDirectory(model));
    for (size_t i = 0; i < files.size(); ++i)
    {
        const OnnxModelFileSpec &spec = files[i];
        const QString targetPath = targetDir.filePath(spec.relativePath);
        const QString stepLabel = QStringLiteral("%1 (%2/%3: %4)")
            .arg(model)
            .arg(i + 1)
            .arg(files.size())
            .arg(spec.relativePath);
        downloader->setDownloadName(stepLabel);
        const bool ok = co_await downloader->download(
            spec.url,
            targetPath,
            stepLabel,
            true
        );
        if (!ok)
        {
            co_return false;
        }
    }

    co_return modelAvailable(model);
}

QCoro::Task<OnnxBackendCreationResult> OnnxController::createBackend(
    QString modelPath)
{
#ifdef HAVE_ASR_BACKEND_ONNX
    OnnxBackendCreationResult result;
    Settings *settings = m_context == nullptr ? nullptr : m_context->settings();
    if (settings == nullptr)
    {
        result.error = tr("ONNX settings are not available.");
        co_return std::move(result);
    }

    QString cfgError;
    const OnnxModelConfig cfg = OnnxModelConfig::loadFromPath(modelPath, cfgError);
    if (!cfg.isValid())
    {
        result.error = cfgError.isEmpty()
            ? tr("ONNX ASR model was not found in: %1").arg(modelPath)
            : cfgError;
        co_return std::move(result);
    }

    OnnxSessionOptions options;
    options.modelPath = std::move(modelPath);
    options.useGpu = settings->onnxUseGpu();
    options.gpuDevice = settings->onnxGpuDevice();
    options.threads = settings->onnxThreads();
    options.punctuation = settings->onnxPunctuation();

    auto backend = std::make_shared<OnnxBackend>(std::move(options));
    if (!backend->isLoaded())
    {
        result.error = backend->loadError().isEmpty()
            ? tr("Failed to initialize ONNX Runtime session.")
            : backend->loadError();
        co_return std::move(result);
    }

    result.backend = std::move(backend);
    co_return std::move(result);
#else
    Q_UNUSED(modelPath)
    OnnxBackendCreationResult result;
    result.error = tr(
        "ONNX Runtime subtitle support is not available in this build."
    );
    co_return std::move(result);
#endif
}
