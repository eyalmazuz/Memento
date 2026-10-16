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

#include "asr/audio/melfrontend.h"

#include <algorithm>
#include <cmath>
#include <complex>
#include <numeric>

namespace AsrAudio
{

namespace
{

constexpr double PI = 3.14159265358979323846;

double hzToMelSlaney(double freqHz)
{
    constexpr double fMin = 0.0;
    constexpr double fSp = 200.0 / 3.0;
    constexpr double minLogHz = 1000.0;
    constexpr double minLogMel = (minLogHz - fMin) / fSp;
    const double logStep = std::log(6.4) / 27.0;

    if (freqHz >= minLogHz)
    {
        return minLogMel + std::log(freqHz / minLogHz) / logStep;
    }
    return (freqHz - fMin) / fSp;
}

double melToHzSlaney(double mel)
{
    constexpr double fMin = 0.0;
    constexpr double fSp = 200.0 / 3.0;
    constexpr double minLogHz = 1000.0;
    constexpr double minLogMel = (minLogHz - fMin) / fSp;
    const double logStep = std::log(6.4) / 27.0;

    if (mel >= minLogMel)
    {
        return minLogHz * std::exp(logStep * (mel - minLogMel));
    }
    return fMin + fSp * mel;
}

void radix2FftInPlace(std::vector<std::complex<float>> &a)
{
    const size_t n = a.size();
    for (size_t i = 1, j = 0; i < n; ++i)
    {
        size_t bit = n >> 1;
        for (; j & bit; bit >>= 1)
        {
            j ^= bit;
        }
        j ^= bit;
        if (i < j)
        {
            std::swap(a[i], a[j]);
        }
    }

    for (size_t len = 2; len <= n; len <<= 1)
    {
        const double ang = -2.0 * PI / static_cast<double>(len);
        const std::complex<float> wlen(
            static_cast<float>(std::cos(ang)),
            static_cast<float>(std::sin(ang))
        );
        for (size_t i = 0; i < n; i += len)
        {
            std::complex<float> w(1.0f, 0.0f);
            const size_t half = len >> 1;
            for (size_t j = 0; j < half; ++j)
            {
                const std::complex<float> u = a[i + j];
                const std::complex<float> v = a[i + j + half] * w;
                a[i + j] = u + v;
                a[i + j + half] = u - v;
                w *= wlen;
            }
        }
    }
}

} // namespace

MelFrontend::MelFrontend(MelFrontendConfig config) :
    m_config(config)
{
    initWindow();
    initMelFilterbank();
}

void MelFrontend::initWindow()
{
    m_paddedWindow.assign(static_cast<size_t>(m_config.nFft), 0.0f);
    const int winLen = std::min(m_config.winLength, m_config.nFft);
    const int leftPad = (m_config.nFft - winLen) / 2;
    for (int i = 0; i < winLen; ++i)
    {
        // Symmetric Hann window (periodic=False), matching torch.hann_window(win_length, periodic=False)
        const double denom = winLen > 1 ? static_cast<double>(winLen - 1) : 1.0;
        const double w = 0.5 * (1.0 - std::cos(2.0 * PI * static_cast<double>(i) / denom));
        m_paddedWindow[static_cast<size_t>(leftPad + i)] = static_cast<float>(w);
    }
}

void MelFrontend::initMelFilterbank()
{
    const int numBins = m_config.nFft / 2 + 1;
    const int nMels = m_config.nMels;
    m_melFilters.assign(static_cast<size_t>(nMels * numBins), 0.0f);

    const double fMin = 0.0;
    const double fMax = static_cast<double>(m_config.sampleRate) / 2.0;
    const double melMin = hzToMelSlaney(fMin);
    const double melMax = hzToMelSlaney(fMax);

    std::vector<double> melPts(static_cast<size_t>(nMels + 2));
    for (int i = 0; i < nMels + 2; ++i)
    {
        const double m = melMin + (melMax - melMin) * static_cast<double>(i) / static_cast<double>(nMels + 1);
        melPts[static_cast<size_t>(i)] = melToHzSlaney(m);
    }

    std::vector<double> fftFreqs(static_cast<size_t>(numBins));
    for (int k = 0; k < numBins; ++k)
    {
        fftFreqs[static_cast<size_t>(k)] =
            static_cast<double>(k) * static_cast<double>(m_config.sampleRate) / static_cast<double>(m_config.nFft);
    }

    for (int m = 0; m < nMels; ++m)
    {
        const double fLeft = melPts[static_cast<size_t>(m)];
        const double fCenter = melPts[static_cast<size_t>(m + 1)];
        const double fRight = melPts[static_cast<size_t>(m + 2)];
        const double diffLower = std::max(1e-12, fCenter - fLeft);
        const double diffUpper = std::max(1e-12, fRight - fCenter);
        const double enorm = 2.0 / std::max(1e-12, fRight - fLeft);

        float *row = m_melFilters.data() + static_cast<size_t>(m * numBins);
        for (int k = 0; k < numBins; ++k)
        {
            const double freq = fftFreqs[static_cast<size_t>(k)];
            const double lower = (freq - fLeft) / diffLower;
            const double upper = (fRight - freq) / diffUpper;
            const double weight = std::max(0.0, std::min(lower, upper)) * enorm;
            row[k] = static_cast<float>(weight);
        }
    }
}

MelFeatureOutput MelFrontend::extract(const float *samples, size_t numSamples) const
{
    MelFeatureOutput out;
    out.nMels = m_config.nMels;
    if (samples == nullptr || numSamples == 0 || m_config.hopLength <= 0 || m_config.nFft <= 0)
    {
        return out;
    }

    // 1. Pre-emphasis filter: y[0] = x[0], y[t] = x[t] - preemphasis * x[t-1]
    std::vector<float> preemph(numSamples);
    if (m_config.preemphasis > 0.0f)
    {
        preemph[0] = samples[0];
        for (size_t i = 1; i < numSamples; ++i)
        {
            preemph[i] = samples[i] - m_config.preemphasis * samples[i - 1];
        }
    }
    else
    {
        std::copy(samples, samples + numSamples, preemph.begin());
    }

    // 2. Center constant zero-padding of n_fft / 2 on both sides (matching torch.stft(..., pad_mode="constant"))
    const int pad = m_config.nFft / 2;
    const size_t paddedLen = numSamples + static_cast<size_t>(2 * pad);
    std::vector<float> padded(paddedLen, 0.0f);
    std::copy(preemph.begin(), preemph.end(), padded.begin() + pad);

    const int64_t totalFrames = 1 + static_cast<int64_t>((paddedLen - static_cast<size_t>(m_config.nFft)) / static_cast<size_t>(m_config.hopLength));
    const int64_t validFrames = std::min<int64_t>(
        totalFrames,
        static_cast<int64_t>(numSamples) / m_config.hopLength
    );
    if (totalFrames <= 0 || validFrames <= 0)
    {
        return out;
    }

    out.numFrames = totalFrames;
    out.validFrames = validFrames;
    out.features.assign(static_cast<size_t>(totalFrames * m_config.nMels), 0.0f);
    out.attentionMaskInt64.assign(static_cast<size_t>(totalFrames), 0);
    out.attentionMaskBool.assign(static_cast<size_t>(totalFrames), 0);

    for (int64_t t = 0; t < validFrames; ++t)
    {
        out.attentionMaskInt64[static_cast<size_t>(t)] = 1;
        out.attentionMaskBool[static_cast<size_t>(t)] = 1;
    }

    // 3. STFT power spectrum + Slaney Mel filterbank + log compression
    const int nFft = m_config.nFft;
    const int numBins = nFft / 2 + 1;
    const int nMels = m_config.nMels;
    std::vector<std::complex<float>> fftBuf(static_cast<size_t>(nFft));
    std::vector<float> powerSpec(static_cast<size_t>(numBins));

    for (int64_t t = 0; t < totalFrames; ++t)
    {
        const size_t frameStart = static_cast<size_t>(t * m_config.hopLength);
        for (int i = 0; i < nFft; ++i)
        {
            fftBuf[static_cast<size_t>(i)] = std::complex<float>(
                padded[frameStart + static_cast<size_t>(i)] * m_paddedWindow[static_cast<size_t>(i)],
                0.0f
            );
        }

        radix2FftInPlace(fftBuf);

        for (int k = 0; k < numBins; ++k)
        {
            const float re = fftBuf[static_cast<size_t>(k)].real();
            const float im = fftBuf[static_cast<size_t>(k)].imag();
            powerSpec[static_cast<size_t>(k)] = re * re + im * im;
        }

        float *melFrame = out.features.data() + static_cast<size_t>(t * nMels);
        for (int m = 0; m < nMels; ++m)
        {
            const float *filterRow = m_melFilters.data() + static_cast<size_t>(m * numBins);
            float sum = 0.0f;
            for (int k = 0; k < numBins; ++k)
            {
                sum += filterRow[k] * powerSpec[static_cast<size_t>(k)];
            }
            melFrame[m] = std::log(std::max(0.0f, sum) + m_config.logZeroGuard);
        }
    }

    // 4. Per-feature normalization over validFrames (with Bessel's correction validFrames - 1)
    if (m_config.normalizePerFeature && validFrames > 1)
    {
        for (int m = 0; m < nMels; ++m)
        {
            double sum = 0.0;
            for (int64_t t = 0; t < validFrames; ++t)
            {
                sum += out.features[static_cast<size_t>(t * nMels + m)];
            }
            const double mean = sum / static_cast<double>(validFrames);

            double sqDiffSum = 0.0;
            for (int64_t t = 0; t < validFrames; ++t)
            {
                const double diff = out.features[static_cast<size_t>(t * nMels + m)] - mean;
                sqDiffSum += diff * diff;
            }
            const double variance = sqDiffSum / static_cast<double>(validFrames - 1);
            const float invStd = static_cast<float>(
                1.0 / (std::sqrt(variance) + static_cast<double>(m_config.normEpsilon))
            );
            const float meanF = static_cast<float>(mean);

            for (int64_t t = 0; t < validFrames; ++t)
            {
                float &val = out.features[static_cast<size_t>(t * nMels + m)];
                val = (val - meanF) * invStd;
            }
            for (int64_t t = validFrames; t < totalFrames; ++t)
            {
                out.features[static_cast<size_t>(t * nMels + m)] = 0.0f;
            }
        }
    }
    else
    {
        for (int64_t t = validFrames; t < totalFrames; ++t)
        {
            for (int m = 0; m < nMels; ++m)
            {
                out.features[static_cast<size_t>(t * nMels + m)] = 0.0f;
            }
        }
    }

    return out;
}

std::vector<AudioUtteranceSpan> MelFrontend::segmentSpeechUtterances(
    const float *samples,
    size_t numSamples,
    int sampleRate,
    double maxUtteranceSec)
{
    std::vector<AudioUtteranceSpan> result;
    if (samples == nullptr || numSamples == 0 || sampleRate <= 0)
    {
        return result;
    }

    // 20 ms analysis frames
    const size_t frameLen = static_cast<size_t>( std::max(1, sampleRate / 50) );
    const size_t numFrames = (numSamples + frameLen - 1) / frameLen;
    std::vector<float> rms(numFrames, 0.0f);

    for (size_t f = 0; f < numFrames; ++f)
    {
        const size_t start = f * frameLen;
        const size_t end = std::min(numSamples, start + frameLen);
        double sumSq = 0.0;
        for (size_t i = start; i < end; ++i)
        {
            sumSq += static_cast<double>(samples[i]) * static_cast<double>(samples[i]);
        }
        rms[f] = static_cast<float>(std::sqrt(sumSq / static_cast<double>(end - start)));
    }

    // Compute adaptive RMS threshold from percentiles
    std::vector<float> sortedRms = rms;
    std::sort(sortedRms.begin(), sortedRms.end());
    const float p20 = sortedRms[sortedRms.size() * 20 / 100];
    const float p85 = sortedRms[sortedRms.size() * 85 / 100];
    if (p85 < 0.0015f)
    {
        // Entire clip is near-silent
        return result;
    }

    const float threshold = std::clamp(p20 + 0.12f * (p85 - p20), 0.0025f, 0.025f);
    // Minimum silence gap to split utterances: 14 frames = 280 ms
    constexpr size_t minSilenceFrames = 14;
    // Context padding around speech utterances: 6 frames = 120 ms
    constexpr size_t padFrames = 6;
    // Minimum speech frames to keep an utterance: 8 frames = 160 ms
    constexpr size_t minSpeechFrames = 8;

    struct FrameSpan
    {
        size_t startFrame;
        size_t endFrame;
    };
    std::vector<FrameSpan> rawSpans;

    size_t i = 0;
    while (i < numFrames)
    {
        if (rms[i] < threshold)
        {
            ++i;
            continue;
        }
        const size_t speechStart = i;
        size_t lastActive = i;
        ++i;
        while (i < numFrames)
        {
            if (rms[i] >= threshold)
            {
                lastActive = i;
            }
            else if (i - lastActive >= minSilenceFrames)
            {
                break;
            }
            ++i;
        }
        const size_t speechEnd = lastActive + 1;
        if (speechEnd - speechStart >= minSpeechFrames)
        {
            const size_t paddedStart = speechStart > padFrames ? speechStart - padFrames : 0;
            const size_t paddedEnd = std::min(numFrames, speechEnd + padFrames);
            if (!rawSpans.empty() && paddedStart <= rawSpans.back().endFrame)
            {
                rawSpans.back().endFrame = std::max(rawSpans.back().endFrame, paddedEnd);
            }
            else
            {
                rawSpans.push_back(FrameSpan{paddedStart, paddedEnd});
            }
        }
    }

    // Split any span exceeding maxUtteranceSec at lowest energy valleys
    const size_t maxFrames = static_cast<size_t>(std::max(2.0, maxUtteranceSec) * 50.0);
    const size_t minSplitFrames = static_cast<size_t>(2.0 * 50.0);

    for (const FrameSpan &span : rawSpans)
    {
        size_t curStart = span.startFrame;
        while (span.endFrame - curStart > maxFrames)
        {
            const size_t searchStart = curStart + minSplitFrames;
            const size_t searchEnd = std::min(span.endFrame - minSplitFrames, curStart + maxFrames);
            size_t bestFrame = searchEnd;
            float minWindowEnergy = 1e9f;

            for (size_t f = searchStart; f <= searchEnd; ++f)
            {
                // 5-frame (100 ms) local energy average
                float localSum = 0.0f;
                for (size_t k = (f > 2 ? f - 2 : 0); k <= std::min(numFrames - 1, f + 2); ++k)
                {
                    localSum += rms[k];
                }
                if (localSum < minWindowEnergy)
                {
                    minWindowEnergy = localSum;
                    bestFrame = f;
                }
            }

            const size_t startSample = std::min(numSamples, curStart * frameLen);
            const size_t endSample = std::min(numSamples, bestFrame * frameLen);
            if (endSample > startSample)
            {
                result.push_back(AudioUtteranceSpan{
                    .startSample = startSample,
                    .endSample = endSample,
                    .startSec = static_cast<double>(startSample) / static_cast<double>(sampleRate),
                    .endSec = static_cast<double>(endSample) / static_cast<double>(sampleRate),
                });
            }
            curStart = bestFrame;
        }

        const size_t startSample = std::min(numSamples, curStart * frameLen);
        const size_t endSample = std::min(numSamples, span.endFrame * frameLen);
        if (endSample > startSample)
        {
            result.push_back(AudioUtteranceSpan{
                .startSample = startSample,
                .endSample = endSample,
                .startSec = static_cast<double>(startSample) / static_cast<double>(sampleRate),
                .endSec = static_cast<double>(endSample) / static_cast<double>(sampleRate),
            });
        }
    }

    return result;
}

} // namespace AsrAudio
