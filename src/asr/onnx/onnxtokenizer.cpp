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

#include "asr/onnx/onnxtokenizer.h"

#include <algorithm>

#include <QByteArray>
#include <QFile>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>

namespace
{

bool parseByteFallbackToken(const QString &token, char &outByte)
{
    // Matches "<0xXX>" (length 6)
    if (token.size() != 6 ||
        !token.startsWith(QStringLiteral("<0x")) ||
        !token.endsWith(QLatin1Char('>')))
    {
        return false;
    }
    bool ok = false;
    const uint val = token.mid(3, 2).toUInt(&ok, 16);
    if (!ok || val > 0xFF)
    {
        return false;
    }
    outByte = static_cast<char>(static_cast<unsigned char>(val));
    return true;
}

} // namespace

bool OnnxTokenizer::load(const QString &tokenizerJsonPath, QString &error)
{
    m_idToToken.clear();
    m_tokenToId.clear();
    m_specialTokenIds.clear();

    QFile file(tokenizerJsonPath);
    if (!file.open(QFile::ReadOnly))
    {
        error = QStringLiteral("Could not open tokenizer.json: %1").arg(tokenizerJsonPath);
        return false;
    }

    QJsonParseError parseError;
    const QJsonDocument doc = QJsonDocument::fromJson(file.readAll(), &parseError);
    if (parseError.error != QJsonParseError::NoError || !doc.isObject())
    {
        error = QStringLiteral("Invalid JSON in tokenizer.json: %1").arg(parseError.errorString());
        return false;
    }

    const QJsonObject root = doc.object();
    const QJsonObject modelObj = root.value(QStringLiteral("model")).toObject();
    const QJsonValue vocabVal = modelObj.value(QStringLiteral("vocab"));

    int64_t maxId = -1;
    if (vocabVal.isObject())
    {
        const QJsonObject vocabObj = vocabVal.toObject();
        for (auto it = vocabObj.begin(); it != vocabObj.end(); ++it)
        {
            const int64_t id = it.value().toInteger(-1);
            if (id >= 0)
            {
                maxId = std::max(maxId, id);
            }
        }
        if (maxId >= 0)
        {
            m_idToToken.resize(static_cast<size_t>(maxId + 1));
            m_tokenToId.reserve(static_cast<qsizetype>(maxId + 1));
            for (auto it = vocabObj.begin(); it != vocabObj.end(); ++it)
            {
                const int64_t id = it.value().toInteger(-1);
                if (id >= 0 && static_cast<size_t>(id) < m_idToToken.size())
                {
                    m_idToToken[static_cast<size_t>(id)] = it.key();
                    m_tokenToId.insert(it.key(), id);
                }
            }
        }
    }
    else if (vocabVal.isArray())
    {
        // Unigram / SentencePiece array format: [ ["token", score], ... ]
        const QJsonArray vocabArr = vocabVal.toArray();
        m_idToToken.resize(static_cast<size_t>(vocabArr.size()));
        m_tokenToId.reserve(vocabArr.size());
        for (qsizetype i = 0; i < vocabArr.size(); ++i)
        {
            const QJsonValue entry = vocabArr.at(i);
            QString token;
            if (entry.isArray() && !entry.toArray().isEmpty())
            {
                token = entry.toArray().at(0).toString();
            }
            else if (entry.isString())
            {
                token = entry.toString();
            }
            m_idToToken[static_cast<size_t>(i)] = token;
            m_tokenToId.insert(token, static_cast<int64_t>(i));
        }
    }

    // Parse added_tokens
    const QJsonArray addedTokens = root.value(QStringLiteral("added_tokens")).toArray();
    for (const QJsonValue &val : addedTokens)
    {
        if (!val.isObject())
        {
            continue;
        }
        const QJsonObject obj = val.toObject();
        const int64_t id = obj.value(QStringLiteral("id")).toInteger(-1);
        const QString content = obj.value(QStringLiteral("content")).toString();
        const bool special = obj.value(QStringLiteral("special")).toBool(false);
        if (id < 0)
        {
            continue;
        }
        if (static_cast<size_t>(id) >= m_idToToken.size())
        {
            m_idToToken.resize(static_cast<size_t>(id + 1));
        }
        m_idToToken[static_cast<size_t>(id)] = content;
        m_tokenToId.insert(content, id);
        if (special)
        {
            m_specialTokenIds.insert(id);
        }
    }

    if (m_idToToken.empty())
    {
        error = QStringLiteral("No vocabulary entries found in tokenizer.json.");
        return false;
    }

    return true;
}

int64_t OnnxTokenizer::tokenToId(const QString &token, int64_t fallback) const
{
    auto it = m_tokenToId.find(token);
    return it != m_tokenToId.end() ? it.value() : fallback;
}

QString OnnxTokenizer::idToToken(int64_t id) const
{
    if (id < 0 || static_cast<size_t>(id) >= m_idToToken.size())
    {
        return {};
    }
    return m_idToToken[static_cast<size_t>(id)];
}

bool OnnxTokenizer::isSpecialToken(int64_t id) const
{
    return m_specialTokenIds.contains(id);
}

QString OnnxTokenizer::decode(const std::vector<int64_t> &ids, bool skipSpecialTokens) const
{
    QByteArray utf8Bytes;
    utf8Bytes.reserve(static_cast<qsizetype>(ids.size() * 4));

    // U+2581 LOWER ONE EIGHTH BLOCK ('▁') used by SentencePiece / Metaspace
    static const QString metaspace = QString::fromUtf8("\xE2\x96\x81");

    for (const int64_t id : ids)
    {
        if (id < 0 || static_cast<size_t>(id) >= m_idToToken.size())
        {
            continue;
        }
        if (skipSpecialTokens && m_specialTokenIds.contains(id))
        {
            continue;
        }

        const QString &token = m_idToToken[static_cast<size_t>(id)];
        if (token.isEmpty())
        {
            continue;
        }
        if (skipSpecialTokens &&
            ((token.startsWith(QStringLiteral("<|")) && token.endsWith(QStringLiteral("|>"))) ||
             token == QStringLiteral("<pad>") ||
             token == QStringLiteral("<eos>") ||
             token == QStringLiteral("<unk>") ||
             token == QStringLiteral("<bos>") ||
             token == QStringLiteral("<nospeech>") ||
             token == QStringLiteral("<s>") ||
             token == QStringLiteral("</s>")))
        {
            continue;
        }

        char rawByte = 0;
        if (parseByteFallbackToken(token, rawByte))
        {
            utf8Bytes.append(rawByte);
            continue;
        }

        QString replaced = token;
        replaced.replace(metaspace, QStringLiteral(" "));
        utf8Bytes.append(replaced.toUtf8());
    }

    QString decoded = QString::fromUtf8(utf8Bytes);
    if (decoded.startsWith(QLatin1Char(' ')))
    {
        decoded.remove(0, 1);
    }
    return decoded;
}

std::vector<int64_t> OnnxTokenizer::buildCohereAsrPrompt(
    const QString &language,
    bool punctuation) const
{
    static const QString metaspace = QString::fromUtf8("\xE2\x96\x81");
    const QString langTag = QStringLiteral("<|%1|>").arg(
        language.isEmpty() ? QStringLiteral("ja") : language
    );
    const QString pncTag = punctuation
        ? QStringLiteral("<|pnc|>")
        : QStringLiteral("<|nopnc|>");

    const int64_t spaceId = tokenToId(metaspace, 13764);
    const int64_t startCtxId = tokenToId(QStringLiteral("<|startofcontext|>"), 7);
    const int64_t startTrId = tokenToId(QStringLiteral("<|startoftranscript|>"), 4);
    const int64_t emoUndefId = tokenToId(QStringLiteral("<|emo:undefined|>"), 16);
    const int64_t langId = tokenToId(langTag, 98);
    const int64_t pncId = tokenToId(pncTag, punctuation ? 5 : 6);
    const int64_t noItnId = tokenToId(QStringLiteral("<|noitn|>"), 9);
    const int64_t noTsId = tokenToId(QStringLiteral("<|notimestamp|>"), 11);
    const int64_t noDiarizeId = tokenToId(QStringLiteral("<|nodiarize|>"), 13);

    return {
        spaceId,
        startCtxId,
        startTrId,
        emoUndefId,
        langId,
        langId,
        pncId,
        noItnId,
        noTsId,
        noDiarizeId,
    };
}
