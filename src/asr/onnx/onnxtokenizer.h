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

#include <cstdint>
#include <vector>

#include <QHash>
#include <QSet>
#include <QString>

/**
 * @brief Lightweight tokenizer/detokenizer backed by Hugging Face tokenizer.json.
 *        Supports BPE and Unigram vocabularies, Metaspace (▁), and <0xXX> byte fallback.
 */
class OnnxTokenizer
{
public:
    OnnxTokenizer() = default;

    /**
     * @brief Load vocabulary and special tokens from a Hugging Face tokenizer.json file.
     */
    [[nodiscard]]
    bool load(const QString &tokenizerJsonPath, QString &error);

    [[nodiscard]]
    bool isLoaded() const noexcept
    {
        return !m_idToToken.empty();
    }

    [[nodiscard]]
    size_t vocabSize() const noexcept
    {
        return m_idToToken.size();
    }

    [[nodiscard]]
    int64_t tokenToId(const QString &token, int64_t fallback = -1) const;

    [[nodiscard]]
    QString idToToken(int64_t id) const;

    [[nodiscard]]
    bool isSpecialToken(int64_t id) const;

    /**
     * @brief Detokenize a sequence of token IDs into a UTF-8 QString.
     */
    [[nodiscard]]
    QString decode(const std::vector<int64_t> &ids, bool skipSpecialTokens = true) const;

    /**
     * @brief Build the 10-token decoder prompt for CohereAsr models
     *        ([▁, <|startofcontext|>, <|startoftranscript|>, <|emo:undefined|>,
     *          <|lang|>, <|lang|>, <|pnc/nopnc|>, <|noitn|>, <|notimestamp|>, <|nodiarize|>]).
     */
    [[nodiscard]]
    std::vector<int64_t> buildCohereAsrPrompt(
        const QString &language = QStringLiteral("ja"),
        bool punctuation = true
    ) const;

private:
    std::vector<QString> m_idToToken;
    QHash<QString, int64_t> m_tokenToId;
    QSet<int64_t> m_specialTokenIds;
};
