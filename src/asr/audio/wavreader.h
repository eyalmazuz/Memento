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

#include <vector>

#include <QString>

namespace AsrAudio
{

inline constexpr int SAMPLE_RATE_16K = 16000;

/**
 * @brief Read a 16 kHz mono 16-bit PCM WAV file into normalized float samples.
 *
 * @param path Path to the WAV file.
 * @param error Populated with a human-readable error message on failure.
 * @param expectedSampleRate Expected sample rate in Hz (default 16000).
 * @return Normalized float samples in [-1.0f, 1.0f], or empty on failure.
 */
[[nodiscard]]
std::vector<float> readPcm16Wav(
    const QString &path,
    QString &error,
    int expectedSampleRate = SAMPLE_RATE_16K
);

} // namespace AsrAudio
