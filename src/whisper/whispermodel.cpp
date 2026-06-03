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

#ifdef MEMENTO_WHISPER_SUPPORT

#include "whisper/whispermodel.h"

#include <algorithm>
#include <cmath>
#include <cstring>
#include <utility>
#include <vector>

#include <QByteArray>
#include <QFile>
#include <QtConcurrent>

#include <whisper.h>

static constexpr const char *LANGUAGE_JAPANESE = "ja";
static constexpr int WAV_HEADER_SIZE = 12;
static constexpr int WAV_CHUNK_HEADER_SIZE = 8;
static constexpr int WHISPER_TIME_SCALE = 100;
static constexpr quint16 WAV_FORMAT_PCM = 1;
static constexpr quint16 WAV_FORMAT_FLOAT = 3;

/**
 * @brief Transcription callback context.
 */
struct SegmentCallbackContext
{
    /* Called when a segment is generated */
    std::function<void(const SubtitleEntry &)> callback;
};

/**
 * @brief Decoded WAV audio data.
 */
struct WavAudio
{
    /* Audio sample rate */
    int sampleRate{0};

    /* Audio channel count */
    int channels{0};

    /* Audio samples as interleaved 32-bit floats */
    std::vector<float> samples;
};

/**
 * @brief Read a little-endian unsigned 16-bit integer.
 *
 * @param data The source bytes.
 * @return The integer value.
 */
static quint16 read_u16(const char *data)
{
    return static_cast<quint16>(
        static_cast<quint8>(data[0]) |
        (static_cast<quint8>(data[1]) << 8)
    );
}

/**
 * @brief Read a little-endian unsigned 32-bit integer.
 *
 * @param data The source bytes.
 * @return The integer value.
 */
static quint32 read_u32(const char *data)
{
    return static_cast<quint32>(
        static_cast<quint8>(data[0]) |
        (static_cast<quint8>(data[1]) << 8) |
        (static_cast<quint8>(data[2]) << 16) |
        (static_cast<quint8>(data[3]) << 24)
    );
}

/**
 * @brief Convert one WAV sample to a float.
 *
 * @param data The sample data.
 * @param format The WAV format code.
 * @param bitsPerSample Bits per sample.
 * @return The sample value.
 */
static float read_sample(
    const char *data, quint16 format, quint16 bitsPerSample)
{
    if (format == WAV_FORMAT_FLOAT && bitsPerSample == 32)
    {
        const quint32 bits = read_u32(data);
        float value = 0.0f;
        std::memcpy(&value, &bits, sizeof(value));
        return value;
    }

    if (format != WAV_FORMAT_PCM)
    {
        return 0.0f;
    }

    switch (bitsPerSample)
    {
        case 16:
        {
            const qint16 value = static_cast<qint16>(read_u16(data));
            return static_cast<float>(value) / 32768.0f;
        }

        case 24:
        {
            qint32 value =
                static_cast<quint8>(data[0]) |
                (static_cast<quint8>(data[1]) << 8) |
                (static_cast<quint8>(data[2]) << 16);
            if (value & 0x00800000)
            {
                value |= 0xFF000000;
            }
            return static_cast<float>(value) / 8388608.0f;
        }

        case 32:
        {
            const qint32 value = static_cast<qint32>(read_u32(data));
            return static_cast<float>(value) / 2147483648.0f;
        }

        default:
            return 0.0f;
    }
}

/**
 * @brief Downmix interleaved samples to mono.
 *
 * @param audio The decoded WAV audio.
 * @return Mono samples.
 */
static std::vector<float> downmix_to_mono(const WavAudio &audio)
{
    if (audio.channels <= 0)
    {
        return {};
    }

    const size_t frames = audio.samples.size() / audio.channels;
    std::vector<float> mono(frames);
    for (size_t frame = 0; frame < frames; ++frame)
    {
        float sum = 0.0f;
        for (int channel = 0; channel < audio.channels; ++channel)
        {
            sum += audio.samples[frame * audio.channels + channel];
        }
        mono[frame] = sum / audio.channels;
    }
    return mono;
}

/**
 * @brief Resample mono audio to the whisper.cpp sample rate.
 *
 * @param samples The input samples.
 * @param sampleRate The input sample rate.
 * @return Samples at WHISPER_SAMPLE_RATE.
 */
static std::vector<float> resample_for_whisper(
    const std::vector<float> &samples, int sampleRate)
{
    if (samples.empty() || sampleRate <= 0)
    {
        return {};
    }
    if (sampleRate == WHISPER_SAMPLE_RATE)
    {
        return samples;
    }

    const double outputSize =
        static_cast<double>(samples.size()) *
        WHISPER_SAMPLE_RATE /
        sampleRate;
    std::vector<float> resampled(
        std::max<size_t>(1, static_cast<size_t>(std::llround(outputSize)))
    );
    const double scale = static_cast<double>(sampleRate) / WHISPER_SAMPLE_RATE;
    for (size_t i = 0; i < resampled.size(); ++i)
    {
        const double src = i * scale;
        const size_t index = static_cast<size_t>(src);
        const size_t next = std::min(index + 1, samples.size() - 1);
        const float fraction = static_cast<float>(src - index);
        resampled[i] = samples[index] * (1.0f - fraction) +
            samples[next] * fraction;
    }
    return resampled;
}

/**
 * @brief Decode a WAV file to interleaved float samples.
 *
 * @param path The WAV file path.
 * @return Decoded audio.
 */
static WavAudio read_wav(const QString &path)
{
    QFile file(path);
    if (!file.open(QFile::ReadOnly))
    {
        qWarning("Could not open Whisper audio file.");
        return {};
    }

    const QByteArray bytes = file.readAll();
    if (bytes.size() < WAV_HEADER_SIZE ||
        std::memcmp(bytes.constData(), "RIFF", 4) != 0 ||
        std::memcmp(bytes.constData() + 8, "WAVE", 4) != 0)
    {
        qWarning("Whisper audio file was not a WAV file.");
        return {};
    }

    quint16 format = 0;
    quint16 channels = 0;
    quint32 sampleRate = 0;
    quint16 bitsPerSample = 0;
    QByteArray pcmData;

    qsizetype offset = WAV_HEADER_SIZE;
    while (offset + WAV_CHUNK_HEADER_SIZE <= bytes.size())
    {
        const char *chunk = bytes.constData() + offset;
        const QByteArray id(chunk, 4);
        const quint32 size = read_u32(chunk + 4);
        offset += WAV_CHUNK_HEADER_SIZE;

        if (offset + size > static_cast<quint32>(bytes.size()))
        {
            qWarning("Whisper audio WAV chunk was truncated.");
            return {};
        }

        if (id == "fmt " && size >= 16)
        {
            const char *fmt = bytes.constData() + offset;
            format = read_u16(fmt);
            channels = read_u16(fmt + 2);
            sampleRate = read_u32(fmt + 4);
            bitsPerSample = read_u16(fmt + 14);
        }
        else if (id == "data")
        {
            pcmData = bytes.mid(offset, size);
        }

        offset += size + (size % 2);
    }

    const int bytesPerSample = bitsPerSample / 8;
    if ((format != WAV_FORMAT_PCM && format != WAV_FORMAT_FLOAT) ||
        channels == 0 ||
        sampleRate == 0 ||
        bytesPerSample <= 0 ||
        pcmData.isEmpty())
    {
        qWarning("Whisper audio WAV format is unsupported.");
        return {};
    }

    const int bytesPerFrame = bytesPerSample * channels;
    if (bytesPerFrame <= 0 || pcmData.size() < bytesPerFrame)
    {
        return {};
    }

    WavAudio audio{
        .sampleRate = static_cast<int>(sampleRate),
        .channels = static_cast<int>(channels),
        .samples = {},
    };
    const qsizetype frames = pcmData.size() / bytesPerFrame;
    audio.samples.reserve(frames * channels);
    for (qsizetype frame = 0; frame < frames; ++frame)
    {
        const char *frameData = pcmData.constData() + frame * bytesPerFrame;
        for (int channel = 0; channel < channels; ++channel)
        {
            audio.samples.emplace_back(
                read_sample(
                    frameData + channel * bytesPerSample,
                    format,
                    bitsPerSample
                )
            );
        }
    }
    return audio;
}

/**
 * @brief Load audio samples from a WAV file in whisper.cpp format.
 *
 * @param path The WAV file path.
 * @return 16 kHz mono float samples.
 */
static std::vector<float> read_audio_for_whisper(const QString &path)
{
    const WavAudio audio = read_wav(path);
    return resample_for_whisper(downmix_to_mono(audio), audio.sampleRate);
}

/**
 * @brief Convert a whisper.cpp timestamp to seconds.
 *
 * @param timestamp The whisper.cpp timestamp.
 * @return Seconds.
 */
static double timestamp_to_seconds(int64_t timestamp)
{
    return static_cast<double>(timestamp) / WHISPER_TIME_SCALE;
}

/**
 * @brief Handle whisper.cpp segment callbacks.
 *
 * @param ctx The whisper context.
 * @param state The whisper state.
 * @param nNew The number of new segments.
 * @param userData The callback context.
 */
static void handle_new_segment(
    whisper_context *ctx,
    whisper_state *state,
    int nNew,
    void *userData)
{
    Q_UNUSED(state)

    SegmentCallbackContext *context =
        reinterpret_cast<SegmentCallbackContext *>(userData);
    if (context == nullptr || !context->callback)
    {
        return;
    }

    const int count = whisper_full_n_segments(ctx);
    const int start = std::max(0, count - nNew);
    for (int i = start; i < count; ++i)
    {
        context->callback(
            SubtitleEntry{
                .text = QString::fromUtf8(whisper_full_get_segment_text(ctx, i)),
                .start = timestamp_to_seconds(
                    whisper_full_get_segment_t0(ctx, i)
                ),
                .end = timestamp_to_seconds(
                    whisper_full_get_segment_t1(ctx, i)
                ),
            }
        );
    }
}

/**
 * @brief Handle whisper.cpp abort callbacks.
 *
 * @param userData The abort flag.
 * @return true to abort, false otherwise.
 */
static bool handle_abort(void *userData)
{
    std::atomic_bool *abort = reinterpret_cast<std::atomic_bool *>(userData);
    return abort != nullptr && abort->load();
}

WhisperModel::WhisperModel(
    const QString &modelPath,
    bool useGpu,
    int gpuDevice,
    bool flashAttention)
{
    QWriteLocker locker(&m_modelLock);
    m_model = QtConcurrent::run(
        [modelPath, useGpu, gpuDevice, flashAttention] () -> whisper_context *
        {
            whisper_context_params params = whisper_context_default_params();
            params.use_gpu = useGpu;
            params.gpu_device = std::max(0, gpuDevice);
            params.flash_attn = flashAttention;

            const QByteArray path = modelPath.toUtf8();
            return whisper_init_from_file_with_params(
                path.constData(),
                params
            );
        }
    );
}

WhisperModel::~WhisperModel()
{
    QWriteLocker locker(&m_modelLock);
    whisper_free(getModel());
}

QFuture<WhisperModel::TranscriptionResult> WhisperModel::transcribe(
    const QString &audioPath, Options options)
{
    return QtConcurrent::run(
        [this, audioPath, options = std::move(options)] ()
            -> TranscriptionResult
        {
            std::vector<float> samples = read_audio_for_whisper(audioPath);
            if (samples.empty())
            {
                qWarning("No audio samples available for Whisper.");
                return TranscriptionResult::Failed;
            }

            QReadLocker locker(&m_modelLock);
            whisper_context *ctx = getModel();
            if (ctx == nullptr)
            {
                qWarning("Whisper model is invalid.");
                return TranscriptionResult::Failed;
            }

            const whisper_sampling_strategy strategy =
                options.beamSize > 1 ?
                    WHISPER_SAMPLING_BEAM_SEARCH :
                    WHISPER_SAMPLING_GREEDY;
            whisper_full_params params = whisper_full_default_params(strategy);
            params.n_threads = std::max(1, options.threads);
            params.language = LANGUAGE_JAPANESE;
            params.print_progress = false;
            params.print_realtime = false;
            params.print_special = false;
            params.print_timestamps = false;
            params.translate = false;
            params.greedy.best_of = std::max(1, options.bestOf);
            params.beam_search.beam_size = std::max(1, options.beamSize);

            SegmentCallbackContext callbackContext{
                .callback = options.segmentCallback,
            };
            params.new_segment_callback = handle_new_segment;
            params.new_segment_callback_user_data = &callbackContext;

            std::atomic_bool *abort = options.abort.get();
            params.abort_callback = abort == nullptr ? nullptr : handle_abort;
            params.abort_callback_user_data = abort;

            QByteArray vadModel;
            if (options.useVad)
            {
                vadModel = options.vadModel.toUtf8();
                params.vad = true;
                params.vad_model_path = vadModel.constData();
            }

            const int ret = whisper_full(
                ctx,
                params,
                samples.data(),
                static_cast<int>(samples.size())
            );
            if (ret == 0)
            {
                return TranscriptionResult::Success;
            }
            if (abort != nullptr && abort->load())
            {
                return TranscriptionResult::Canceled;
            }
            else
            {
                qWarning("Whisper transcription failed with code %d.", ret);
            }
            return TranscriptionResult::Failed;
        }
    );
}

inline whisper_context *WhisperModel::getModel() const
{
    return m_model.result();
}

#endif // MEMENTO_WHISPER_SUPPORT
