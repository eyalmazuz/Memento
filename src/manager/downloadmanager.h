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

#include <QObject>
#include <QNetworkAccessManager>
#include <QNetworkReply>
#include <QPointer>
#include <QString>
#include <QUrl>
#include <memory>

#ifdef MEMENTO_SYSTEM_QCORO
#include <QCoroTask>
#else
#include <qcoro/qcorotask.h>
#endif

class Context;

/**
 * @brief Class responsible for downloading files/models in the background.
 */
class DownloadManager : public QObject
{
    Q_OBJECT

    Q_PROPERTY(
        bool downloadRunning
        READ downloadRunning
        NOTIFY downloadRunningChanged
    )

    Q_PROPERTY(
        qreal downloadProgress
        READ downloadProgress
        NOTIFY downloadProgressChanged
    )

    Q_PROPERTY(
        qint64 downloadReceived
        READ downloadReceived
        NOTIFY downloadProgressChanged
    )

    Q_PROPERTY(
        qint64 downloadTotal
        READ downloadTotal
        NOTIFY downloadProgressChanged
    )

    Q_PROPERTY(
        qint64 downloadSpeed
        READ downloadSpeed
        NOTIFY downloadProgressChanged
    )

    Q_PROPERTY(
        QString downloadName
        READ downloadName
        NOTIFY downloadNameChanged
    )

public:
    explicit DownloadManager(Context *context, QObject *parent = nullptr);
    virtual ~DownloadManager();

    [[nodiscard]]
    bool downloadRunning() const noexcept;

    [[nodiscard]]
    qreal downloadProgress() const noexcept;

    [[nodiscard]]
    qint64 downloadReceived() const noexcept;

    [[nodiscard]]
    qint64 downloadTotal() const noexcept;

    [[nodiscard]]
    qint64 downloadSpeed() const noexcept;

    [[nodiscard]]
    QString downloadName() const;

    /**
     * @brief Download a file from a URL to a local path.
     */
    QCoro::Task<bool> download(
        const QUrl &url,
        const QString &path,
        const QString &name,
        bool reportProgress = true
    );

    void setDownloadName(QString value);
    void setDownloadProgress(qint64 received, qint64 total, qint64 speed);
    void setDownloadRunning(bool value);

signals:
    void downloadRunningChanged();
    void downloadProgressChanged();
    void downloadNameChanged();

private:
    Context *m_context{nullptr};
    std::unique_ptr<QNetworkAccessManager> m_manager;
    bool m_downloadRunning{false};
    qreal m_downloadProgress{0.0};
    qint64 m_downloadReceived{0};
    qint64 m_downloadTotal{0};
    qint64 m_downloadSpeed{0};
    QString m_downloadName;
};
