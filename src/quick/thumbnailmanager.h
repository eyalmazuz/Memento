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
#include <QString>
#include <QMutex>
#include <QWaitCondition>
#include <QImage>
#include <mpv/client.h>
#include <mpv/render.h>

/**
 * @brief Manages a separate mpv instance for offscreen software thumbnail rendering.
 */
class ThumbnailManager : public QObject
{
    Q_OBJECT
    Q_PROPERTY(QString source READ source WRITE setSource NOTIFY sourceChanged)

public:
    explicit ThumbnailManager(QObject *parent = nullptr);
    virtual ~ThumbnailManager();

    QString source() const;
    void setSource(const QString &source);

    QImage renderThumbnail(double time, const QSize &requestedSize);
    void onFrameReady();

signals:
    void sourceChanged(const QString &source);

private:
    void initMpv();
    void cleanupMpv();

    mpv_handle *m_mpv{nullptr};
    mpv_render_context *m_mpv_gl{nullptr};

    QString m_source;
    mutable QMutex m_mutex;
    QWaitCondition m_waitCondition;
    QWaitCondition m_cleanupCondition;
    int m_activeRequests{0};
    bool m_frameReady{false};
    bool m_isCleaningUp{false};
};
