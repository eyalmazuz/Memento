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

#include "manager/downloadmanager.h"
#include "state/context.h"
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QNetworkRequest>
#include <QElapsedTimer>

#ifdef MEMENTO_SYSTEM_QCORO
#include <QCoroNetworkReply>
#else
#include <qcoro/network/qcoronetworkreply.h>
#endif

DownloadManager::DownloadManager(Context *context, QObject *parent) :
    QObject(parent),
    m_context(context),
    m_manager(std::make_unique<QNetworkAccessManager>(this))
{
}

DownloadManager::~DownloadManager() = default;

bool DownloadManager::downloadRunning() const noexcept
{
    return m_downloadRunning;
}

qreal DownloadManager::downloadProgress() const noexcept
{
    return m_downloadProgress;
}

qint64 DownloadManager::downloadReceived() const noexcept
{
    return m_downloadReceived;
}

qint64 DownloadManager::downloadTotal() const noexcept
{
    return m_downloadTotal;
}

qint64 DownloadManager::downloadSpeed() const noexcept
{
    return m_downloadSpeed;
}

QString DownloadManager::downloadName() const
{
    return m_downloadName;
}

void DownloadManager::setDownloadRunning(bool value)
{
    if (m_downloadRunning != value)
    {
        m_downloadRunning = value;
        emit downloadRunningChanged();
    }
}

void DownloadManager::setDownloadName(QString value)
{
    if (m_downloadName != value)
    {
        m_downloadName = std::move(value);
        emit downloadNameChanged();
    }
}

void DownloadManager::setDownloadProgress(qint64 received, qint64 total, qint64 speed)
{
    const qreal progress = total > 0 ? static_cast<qreal>(received) / total : 0.0;
    if (!qFuzzyCompare(m_downloadProgress, progress) ||
        m_downloadReceived != received ||
        m_downloadTotal != total ||
        m_downloadSpeed != speed)
    {
        m_downloadProgress = progress;
        m_downloadReceived = received;
        m_downloadTotal = total;
        m_downloadSpeed = speed;
        emit downloadProgressChanged();
    }
}

QCoro::Task<bool> DownloadManager::download(
    const QUrl &url,
    const QString &path,
    const QString &name,
    bool reportProgress)
{
    if (!url.isValid() || path.isEmpty())
    {
        co_return false;
    }
    if (QFileInfo::exists(path))
    {
        if (reportProgress)
        {
            const qint64 size = QFileInfo(path).size();
            setDownloadName(name);
            setDownloadProgress(size, size, 0);
        }
        co_return true;
    }

    const QFileInfo info(path);
    if (!QDir().mkpath(info.absolutePath()))
    {
        qWarning("Could not create download target directory.");
        co_return false;
    }

    const QString partialPath = path + ".download";
    QFile::remove(partialPath);
    QFile file(partialPath);
    if (!file.open(QFile::WriteOnly))
    {
        qWarning("Could not open download target file.");
        co_return false;
    }

    QNetworkRequest req{url};
    req.setAttribute(
        QNetworkRequest::RedirectPolicyAttribute,
        QNetworkRequest::UserVerifiedRedirectPolicy
    );
    std::unique_ptr<QNetworkReply> reply{m_manager->get(std::move(req))};
    connect(
        reply.get(), &QNetworkReply::redirected,
        reply.get(), &QNetworkReply::redirectAllowed
    );
    QElapsedTimer timer;
    timer.start();
    if (reportProgress)
    {
        connect(
            reply.get(),
            &QNetworkReply::downloadProgress,
            this,
            [this, &timer] (qint64 received, qint64 total)
            {
                const qint64 elapsed = qMax<qint64>(1, timer.elapsed());
                setDownloadProgress(
                    received,
                    total,
                    (received * 1000) / elapsed
                );
            }
        );
    }
    connect(
        reply.get(), &QNetworkReply::readyRead,
        reply.get(),
        [&file, reply = reply.get()]
        {
            file.write(reply->readAll());
        }
    );
    co_await reply.get();
    file.write(reply->readAll());
    file.close();

    const QVariant statusCode = reply->attribute(
        QNetworkRequest::HttpStatusCodeAttribute
    );
    if (reply->error() != QNetworkReply::NetworkError::NoError ||
        (statusCode.isValid() && statusCode.toInt() >= 400))
    {
        qWarning(
            "Download failed: %s",
            qUtf8Printable(reply->errorString())
        );
        QFile::remove(partialPath);
        co_return false;
    }

    QFile::remove(path);
    if (!QFile::rename(partialPath, path))
    {
        qWarning("Could not finalize downloaded file.");
        QFile::remove(partialPath);
        co_return false;
    }

    if (reportProgress)
    {
        const qint64 size = QFileInfo(path).size();
        setDownloadProgress(size, size, 0);
    }
    co_return true;
}
