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

#include <QFile>
#include <QImage>
#include <QStringList>
#include <QThread>

namespace
{

constexpr qsizetype THUMBNAIL_WIDTH_INDEX = 1;
constexpr qsizetype THUMBNAIL_HEIGHT_INDEX = 2;
constexpr qsizetype THUMBNAIL_PATH_INDEX = 3;
static constexpr int THUMBNAIL_RETRY_COUNT = 3;
static constexpr unsigned long THUMBNAIL_RETRY_DELAY_MS = 5;

/* Begin Static Functions */

/**
 * @brief Parses a thumbnail provider image id.
 *
 * @param id The image id passed by QML.
 * @param[out] width The parsed raw thumbnail width.
 * @param[out] height The parsed raw thumbnail height.
 * @param[out] path The parsed raw thumbnail file path.
 * @return true if the id is valid,
 * @return false otherwise.
 */
[[nodiscard]]
bool parseThumbnailId(
    const QString &id,
    int &width,
    int &height,
    QString &path)
{
    QStringList parts = id.split('/', Qt::KeepEmptyParts);
    if (parts.size() <= THUMBNAIL_PATH_INDEX)
    {
        return false;
    }

    bool widthOkay = false;
    bool heightOkay = false;
    width = parts[THUMBNAIL_WIDTH_INDEX].toInt(&widthOkay);
    height = parts[THUMBNAIL_HEIGHT_INDEX].toInt(&heightOkay);
    path = parts.mid(THUMBNAIL_PATH_INDEX).join('/');
    return widthOkay && heightOkay && width > 0 && height > 0 &&
        !path.isEmpty();
}

/**
 * @brief Reads thumbnail bytes from a path, retrying short write races.
 *
 * @param path The raw thumbnail path to read.
 * @param expectedSize The minimum number of bytes expected.
 * @return The thumbnail bytes. Empty if the file could not be read.
 */
[[nodiscard]]
QByteArray readThumbnailData(const QString &path, qint64 expectedSize)
{
    QStringList paths{path};
    if (!path.endsWith(".bgra"))
    {
        paths.append(path + ".bgra");
    }

    for (int retry = 0; retry < THUMBNAIL_RETRY_COUNT; ++retry)
    {
        for (const QString &candidate : paths)
        {
            QFile file(candidate);
            if (!file.open(QIODevice::ReadOnly))
            {
                continue;
            }

            QByteArray data = file.readAll();
            if (data.size() >= expectedSize)
            {
                return data;
            }
        }

        QThread::msleep(THUMBNAIL_RETRY_DELAY_MS);
    }

    return {};
}

}

/* End Static Functions */

/* Begin Constructor */

ThumbnailProvider::ThumbnailProvider()
    : QQuickImageProvider(QQuickImageProvider::Image)
{
}

/* End Constructor */

/* Begin Public Functions */

QImage ThumbnailProvider::requestImage(
    const QString &id,
    QSize *size,
    const QSize &requestedSize)
{
    Q_UNUSED(requestedSize);

    int width = 0;
    int height = 0;
    QString path;
    if (!parseThumbnailId(id, width, height, path))
    {
        return QImage();
    }

    qint64 expectedSize = static_cast<qint64>(width) * height * 4;
    QByteArray data = readThumbnailData(path, expectedSize);
    if (data.isEmpty())
    {
        return QImage();
    }

    /* Format_ARGB32 matches little-endian BGRA memory layout natively */
    QImage image(
        reinterpret_cast<const uchar *>(data.constData()),
        width,
        height,
        QImage::Format_ARGB32
    );

    if (size)
    {
        *size = image.size();
    }

    /* Copy the image because it points to the temporary QByteArray data. */
    return image.copy();
}

/* End Public Functions */
