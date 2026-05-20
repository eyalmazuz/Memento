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

#include "thumbnailprovider.h"
#include "thumbnailmanager.h"

ThumbnailProvider::ThumbnailProvider(ThumbnailManager *manager)
    : QQuickImageProvider(QQuickImageProvider::Image)
    , m_manager(manager)
{
}

QImage ThumbnailProvider::requestImage(
    const QString &id,
    QSize *size,
    const QSize &requestedSize)
{
    // The id contains the timestamp, e.g. "123.45" or "123.45?u=1"
    QString timeStr = id;
    int queryIndex = id.indexOf('?');
    if (queryIndex != -1)
    {
        timeStr = id.left(queryIndex);
    }

    bool ok = false;
    double time = timeStr.toDouble(&ok);
    if (!ok || !m_manager)
    {
        return QImage();
    }

    QImage image = m_manager->renderThumbnail(time, requestedSize);
    if (size)
    {
        *size = image.size();
    }

    return image;
}
