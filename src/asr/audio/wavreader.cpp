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

#include "asr/audio/wavreader.h"

#include <cstring>

#include <QByteArray>
#include <QFile>

namespace AsrAudio
{

namespace
{

constexpr int WAV_HEADER_SIZE = 12;
constexpr int WAV_CHUNK_HEADER_SIZE = 8;
constexpr quint16 WAV_FORMAT_PCM = 1;
constexpr quint16 WAV_BITS_PER_SAMPLE = 16;
constexpr quint16 WAV_CHANNELS = 1;

quint16 readU16(const char *data)
{
    return static_cast<quint16>(
        static_cast<quint8>(data[0]) |
        (static_cast<quint8>(data[1]) << 8)
    );
}

quint32 readU32(const char *data)
{
    return static_cast<quint32>(
        static_cast<quint8>(data[0]) |
        (static_cast<quint8>(data[1]) << 8) |
        (static_cast<quint8>(data[2]) << 16) |
        (static_cast<quint8>(data[3]) << 24)
    );
}

} // namespace

std::vector<float> readPcm16Wav(
    const QString &path,
    QString &error,
    int expectedSampleRate)
{
    const auto fail = [&error](const QString &message)
    {
        error = message;
        qWarning("%s", qUtf8Printable(message));
    };

    QFile file(path);
    if (!file.open(QFile::ReadOnly))
    {
        fail(QStringLiteral("Could not open the extracted audio file."));
        return {};
    }

    const QByteArray bytes = file.readAll();
    if (bytes.size() < WAV_HEADER_SIZE)
    {
        fail(QStringLiteral("The extracted WAV is truncated (%1 bytes).")
            .arg(bytes.size()));
        return {};
    }
    if (std::memcmp(bytes.constData(), "RIFF", 4) != 0 ||
        std::memcmp(bytes.constData() + 8, "WAVE", 4) != 0)
    {
        fail(QStringLiteral("The extracted audio is not RIFF/WAVE (header %1).")
            .arg(QString::fromLatin1(bytes.first(12).toHex(' '))));
        return {};
    }

    quint16 format = 0;
    quint16 channels = 0;
    quint32 sampleRate = 0;
    quint16 blockAlign = 0;
    quint16 bitsPerSample = 0;
    QByteArray pcmData;

    qsizetype offset = WAV_HEADER_SIZE;
    while (offset + WAV_CHUNK_HEADER_SIZE <= bytes.size())
    {
        const char *chunk = bytes.constData() + offset;
        const QByteArray id(chunk, 4);
        const quint32 size = readU32(chunk + 4);
        offset += WAV_CHUNK_HEADER_SIZE;

        if (offset + size > static_cast<quint32>(bytes.size()))
        {
            fail(QStringLiteral("The extracted WAV file is truncated."));
            return {};
        }

        if (id == "fmt " && size >= 16)
        {
            const char *fmt = bytes.constData() + offset;
            format = readU16(fmt);
            channels = readU16(fmt + 2);
            sampleRate = readU32(fmt + 4);
            blockAlign = readU16(fmt + 12);
            bitsPerSample = readU16(fmt + 14);
        }
        else if (id == "data")
        {
            pcmData = bytes.mid(offset, size);
        }

        offset += size + (size % 2);
    }

    constexpr int BYTES_PER_SAMPLE = WAV_BITS_PER_SAMPLE / 8;
    if (format != WAV_FORMAT_PCM ||
        channels != WAV_CHANNELS ||
        sampleRate != static_cast<quint32>(expectedSampleRate) ||
        blockAlign != WAV_CHANNELS * BYTES_PER_SAMPLE ||
        bitsPerSample != WAV_BITS_PER_SAMPLE ||
        pcmData.isEmpty())
    {
        fail(QStringLiteral("The extracted WAV is not 16 kHz mono PCM16."));
        return {};
    }

    if (pcmData.size() < BYTES_PER_SAMPLE)
    {
        fail(QStringLiteral("The extracted WAV contains no audio samples."));
        return {};
    }

    const qsizetype samples = pcmData.size() / BYTES_PER_SAMPLE;
    std::vector<float> output;
    output.reserve(samples);
    for (qsizetype i = 0; i < samples; ++i)
    {
        const qint16 value = static_cast<qint16>(
            readU16(pcmData.constData() + i * BYTES_PER_SAMPLE)
        );
        output.emplace_back(static_cast<float>(value) / 32768.0f);
    }
    return output;
}

} // namespace AsrAudio
