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

#include <cstddef>
#include <cstdint>
#include <vector>

namespace AsrAudio
{

/**
 * @brief Configuration for log-Mel spectrogram extraction (CohereAsr / NeMo Parakeet compatible).
 */
struct MelFrontendConfig
{
    int sampleRate{16000};
    int nFft{512};
    int winLength{400};
    int hopLength{160};
    int nMels{128};
    float preemphasis{0.97f};
    float logZeroGuard{5.9604644775390625e-8f}; // 2^-24
    float normEpsilon{1e-5f};
    bool normalizePerFeature{true};
};

/**
 * @brief Extracted log-Mel spectrogram features and attention masks for a single utterance/chunk.
 */
struct MelFeatureOutput
{
    /* Row-major [numFrames, nMels] float32 features */
    std::vector<float> features;

    /* [numFrames] attention mask (1 for valid frames, 0 for padded frames) */
    std::vector<int64_t> attentionMaskInt64;
    std::vector<uint8_t> attentionMaskBool;

    int64_t numFrames{0};
    int64_t validFrames{0};
    int64_t nMels{0};
};

/**
 * @brief Time-aligned speech utterance span within a longer audio buffer.
 */
struct AudioUtteranceSpan
{
    size_t startSample{0};
    size_t endSample{0};
    double startSec{0.0};
    double endSec{0.0};
};

/**
 * @brief Configurable STFT + Slaney Mel filterbank frontend.
 */
class MelFrontend
{
public:
    explicit MelFrontend(MelFrontendConfig config = {});

    [[nodiscard]]
    const MelFrontendConfig &config() const noexcept
    {
        return m_config;
    }

    /**
     * @brief Extract log-Mel spectrogram features from 16 kHz mono float PCM samples.
     */
    [[nodiscard]]
    MelFeatureOutput extract(const float *samples, size_t numSamples) const;

    [[nodiscard]]
    MelFeatureOutput extract(const std::vector<float> &samples) const
    {
        return extract(samples.data(), samples.size());
    }

    /**
     * @brief Segment a long PCM buffer into speech utterances using short-time RMS energy
     *        so untimed encoder-decoder models emit accurately timed subtitle cues.
     */
    [[nodiscard]]
    static std::vector<AudioUtteranceSpan> segmentSpeechUtterances(
        const float *samples,
        size_t numSamples,
        int sampleRate = 16000,
        double maxUtteranceSec = 10.0
    );

private:
    void initWindow();
    void initMelFilterbank();

    MelFrontendConfig m_config;
    std::vector<float> m_paddedWindow;
    /* Row-major [nMels, numFreqBins] where numFreqBins = nFft / 2 + 1 */
    std::vector<float> m_melFilters;
};

} // namespace AsrAudio
