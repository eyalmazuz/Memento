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

#include "thumbnailmanager.h"

#include <QMutexLocker>
#include <QDebug>

namespace
{

static void onMpvUpdate(void *ctx)
{
    if (auto *manager = static_cast<ThumbnailManager *>(ctx))
    {
        manager->onFrameReady();
    }
}

}

ThumbnailManager::ThumbnailManager(QObject *parent)
    : QObject(parent)
{
}

ThumbnailManager::~ThumbnailManager()
{
    cleanupMpv();
}

QString ThumbnailManager::source() const
{
    QMutexLocker locker(&m_mutex);
    return m_source;
}

void ThumbnailManager::setSource(const QString &source)
{
    bool changed = false;
    {
        QMutexLocker locker(&m_mutex);
        if (m_source != source)
        {
            m_source = source;
            changed = true;
        }
    }

    if (changed)
    {
        emit sourceChanged(source);
    }
    else
    {
        return;
    }

    if (source.isEmpty())
    {
        cleanupMpv();
        return;
    }

    initMpv();

    mpv_handle *mpv = nullptr;
    {
        QMutexLocker locker(&m_mutex);
        mpv = m_mpv;
    }

    if (mpv)
    {
        const char *cmd[] = {"loadfile", source.toUtf8().constData(), nullptr};
        ::mpv_command(mpv, cmd);
    }
}

QImage ThumbnailManager::renderThumbnail(double time, const QSize &requestedSize)
{
    {
        QMutexLocker locker(&m_mutex);
        if (m_isCleaningUp || !m_mpv || !m_mpv_gl || m_source.isEmpty())
        {
            return QImage();
        }
        m_activeRequests++;
    }

    mpv_handle *mpv = nullptr;
    mpv_render_context *mpv_gl = nullptr;
    {
        QMutexLocker locker(&m_mutex);
        mpv = m_mpv;
        mpv_gl = m_mpv_gl;
    }

    if (!mpv || !mpv_gl)
    {
        QMutexLocker locker(&m_mutex);
        m_activeRequests--;
        if (m_activeRequests == 0)
        {
            m_cleanupCondition.wakeAll();
        }
        return QImage();
    }

    int64_t videoWidth = 0;
    int getPropErr = ::mpv_get_property(mpv, "video-params/w", MPV_FORMAT_INT64, &videoWidth);
    if (getPropErr >= 0 && videoWidth <= 0)
    {
        QMutexLocker locker(&m_mutex);
        m_activeRequests--;
        if (m_activeRequests == 0)
        {
            m_cleanupCondition.wakeAll();
        }
        return QImage();
    }

    {
        QMutexLocker locker(&m_mutex);
        m_frameReady = false;
    }

    QString seekCmd = QString("seek %1 absolute").arg(time);
    ::mpv_command_string(mpv, seekCmd.toUtf8().constData());

    QMutexLocker locker(&m_mutex);
    while (!m_frameReady && !m_isCleaningUp)
    {
        if (!m_waitCondition.wait(&m_mutex, 500))
        {
            break;
        }
    }

    QImage image;
    if (m_frameReady && !m_isCleaningUp && m_mpv_gl)
    {
        int width = 320;
        int height = 180;
        if (requestedSize.isValid())
        {
            width = requestedSize.width();
            height = requestedSize.height();
        }

        image = QImage(width, height, QImage::Format_RGB32);
        int stride = image.bytesPerLine();
        uchar *bits = image.bits();

        int size[2] = {width, height};
        mpv_render_param params[] = {
            {MPV_RENDER_PARAM_SW_SIZE, size},
            {MPV_RENDER_PARAM_SW_FORMAT, const_cast<char *>("bgra")},
            {MPV_RENDER_PARAM_SW_STRIDE, &stride},
            {MPV_RENDER_PARAM_SW_POINTER, bits},
            {MPV_RENDER_PARAM_INVALID, nullptr}
        };

        int err = ::mpv_render_context_render(m_mpv_gl, params);
        if (err < 0)
        {
            image = QImage();
        }
    }

    m_activeRequests--;
    if (m_activeRequests == 0)
    {
        m_cleanupCondition.wakeAll();
    }

    return image;
}

void ThumbnailManager::onFrameReady()
{
    QMutexLocker locker(&m_mutex);
    m_frameReady = true;
    m_waitCondition.wakeAll();
}

void ThumbnailManager::initMpv()
{
    QMutexLocker locker(&m_mutex);
    if (m_mpv)
    {
        return;
    }

    mpv_handle *mpv = ::mpv_create();
    if (!mpv)
    {
        return;
    }

    ::mpv_set_option_string(mpv, "vo", "libmpv");
    ::mpv_set_option_string(mpv, "audio", "no");
    ::mpv_set_option_string(mpv, "sub", "no");
    ::mpv_set_option_string(mpv, "sid", "no");
    ::mpv_set_option_string(mpv, "osd-level", "0");
    ::mpv_set_option_string(mpv, "osc", "no");
    ::mpv_set_option_string(mpv, "pause", "yes");
    ::mpv_set_option_string(mpv, "ytdl", "no");
    ::mpv_set_option_string(mpv, "config", "no");
    ::mpv_set_option_string(mpv, "input-default-bindings", "no");
    ::mpv_set_option_string(mpv, "input-vo-keyboard", "no");
    ::mpv_set_option_string(mpv, "load-stats-overlay", "no");

    locker.unlock();
    int err = ::mpv_initialize(mpv);
    if (err < 0)
    {
        ::mpv_terminate_destroy(mpv);
        return;
    }

    mpv_render_context *mpv_gl = nullptr;
    mpv_render_param params[] = {
        {MPV_RENDER_PARAM_API_TYPE, const_cast<char *>(MPV_RENDER_API_TYPE_SW)},
        {MPV_RENDER_PARAM_INVALID, nullptr}
    };

    err = ::mpv_render_context_create(&mpv_gl, mpv, params);
    if (err < 0)
    {
        ::mpv_terminate_destroy(mpv);
        return;
    }

    ::mpv_render_context_set_update_callback(mpv_gl, onMpvUpdate, this);

    locker.relock();
    m_mpv = mpv;
    m_mpv_gl = mpv_gl;
}

void ThumbnailManager::cleanupMpv()
{
    mpv_handle *mpv = nullptr;
    mpv_render_context *mpv_gl = nullptr;

    {
        QMutexLocker locker(&m_mutex);
        m_isCleaningUp = true;
        m_waitCondition.wakeAll();

        while (m_activeRequests > 0)
        {
            m_cleanupCondition.wait(&m_mutex);
        }

        mpv = m_mpv;
        mpv_gl = m_mpv_gl;
        m_mpv = nullptr;
        m_mpv_gl = nullptr;
        m_isCleaningUp = false;
    }

    if (mpv_gl)
    {
        ::mpv_render_context_set_update_callback(mpv_gl, nullptr, nullptr);
        ::mpv_render_context_free(mpv_gl);
    }
    if (mpv)
    {
        ::mpv_terminate_destroy(mpv);
    }
}
