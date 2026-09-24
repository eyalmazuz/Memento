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

#include "audio/audiofiles.h"

#include <QJsonDocument>
#include <QJsonObject>
#include <QNetworkReply>

AudioFiles::AudioFiles(QObject *parent) : QObject(parent)
{
    connect(
        this, &AudioFiles::settingsChanged,
        this, &AudioFiles::loadFiles
    );
    connect(
        this, &AudioFiles::termChanged,
        this, &AudioFiles::loadFiles
    );
}

AudioFiles::~AudioFiles()
{
    qDeleteAll(m_files);
    m_files.clear();
}

const Settings *AudioFiles::settings() const noexcept
{
    return m_settings.data();
}

void AudioFiles::setSettings(const Settings *value)
{
    if (m_settings == value)
    {
        return;
    }
    QObject::disconnect(m_settingsDestroyedConnection);
    m_settings = value;
    if (value != nullptr)
    {
        m_settingsDestroyedConnection = connect(
            value, &QObject::destroyed, this,
            [this] () -> void
            {
                m_settings.clear();
                emit settingsChanged(nullptr);
            }
        );
    }
    emit settingsChanged(m_settings.data());
}

const Term *AudioFiles::term() const noexcept
{
    return m_term.data();
}

void AudioFiles::setTerm(const Term *value)
{
    if (m_term == value)
    {
        return;
    }
    QObject::disconnect(m_termDestroyedConnection);
    m_term = value;
    if (value != nullptr)
    {
        m_termDestroyedConnection = connect(
            value, &QObject::destroyed, this,
            [this] () -> void
            {
                m_term.clear();
                emit termChanged(nullptr);
            }
        );
    }
    emit termChanged(m_term.data());
}

AudioFiles::LoadState AudioFiles::loadState() const noexcept
{
    return m_loadState;
}

void AudioFiles::setLoadState(LoadState value)
{
    if (m_loadState == value)
    {
        return;
    }
    m_loadState = value;
    emit loadStateChanged(m_loadState);
}

QQmlListProperty<AudioFile> AudioFiles::files()
{
    return QQmlListProperty<AudioFile>(this, &m_files);
}

void AudioFiles::loadFiles()
{
    const quint64 generation = ++m_loadGeneration;
    setLoadState(AudioFiles::LoadState::Unloaded);

    QList<AudioFile *> oldFiles;
    m_files.swap(oldFiles);
    emit filesChanged();
    qDeleteAll(oldFiles);
    oldFiles.clear();

    if (m_term == nullptr || m_settings == nullptr)
    {
        return;
    }

    setLoadState(AudioFiles::LoadState::Loading);

    m_sources = m_settings->audioSources()->items();
    for (AudioSource &source : m_sources)
    {
        source.url
            .replace("{expression}", m_term->expression())
            .replace("{term}", m_term->expression())
            .replace(
                "{reading}",
                m_term->reading().isEmpty() ?
                    m_term->expression() : m_term->reading()
            );
    }
    m_sourceCurrent = 0;

    loadCurrentSource(generation);
}

void AudioFiles::loadCurrentSource(quint64 generation)
{
    if (generation != m_loadGeneration)
    {
        return;
    }

    if (m_term == nullptr || m_settings == nullptr)
    {
        loadFiles();
        return;
    }

    while (m_sourceCurrent < m_sources.size())
    {
        switch (m_sources[m_sourceCurrent].type)
        {
            case Setting::AudioSourceTypeFile:
            {
                AudioFile *file = new AudioFile(this);
                file->setName(m_sources[m_sourceCurrent].name);
                file->setUrl(m_sources[m_sourceCurrent].url);
                file->setSkipHash(m_sources[m_sourceCurrent].skipHash);
                file->setExists(true);
                m_files.emplaceBack(file);
                break;
            }

            case Setting::AudioSourceTypeJson:
                loadJsonAudioSource(generation);
                return;
        }

        ++m_sourceCurrent;
    }

    setLoadState(AudioFiles::LoadState::Loaded);
    emit filesChanged();
}

void AudioFiles::loadJsonAudioSource(quint64 generation)
{
    const qsizetype sourceCurrent = m_sourceCurrent++;
    const QString skipHash = m_sources[sourceCurrent].skipHash;

    QNetworkReply *reply =
        m_manager.get(QNetworkRequest(QUrl(m_sources[sourceCurrent].url)));
    connect(
        reply, &QNetworkReply::finished, this,
        [this, reply, generation, skipHash] () -> void
        {
            if (generation != m_loadGeneration)
            {
                reply->deleteLater();
                return;
            }

            if (reply->error() != QNetworkReply::NetworkError::NoError)
            {
                reply->deleteLater();
                loadCurrentSource(generation);
                return;
            }

            QByteArray dataBytes;
            if (reply->isOpen() && reply->isReadable())
            {
                dataBytes = reply->readAll();
            }
            reply->deleteLater();
            QJsonParseError jsonParseError{};
            QJsonDocument data =
                QJsonDocument::fromJson(dataBytes, &jsonParseError);
            if (jsonParseError.error != QJsonParseError::NoError)
            {
                loadCurrentSource(generation);
                return;
            }

            if (!data.isObject())
            {
                loadCurrentSource(generation);
                return;
            }
            QJsonObject obj = data.object();
            if (obj["type"].toString() != "audioSourceList")
            {
                loadCurrentSource(generation);
                return;
            }
            QJsonArray arr = obj["audioSources"].toArray();
            for (QJsonValueConstRef value : arr)
            {
                if (!value.isObject())
                {
                    continue;
                }
                QJsonObject obj = value.toObject();

                QJsonValueConstRef name = obj["name"];
                if (!name.isString())
                {
                    continue;
                }

                QJsonValueConstRef url = obj["url"];
                if (!url.isString())
                {
                    continue;
                }

                AudioFile *file = new AudioFile(this);
                file->setName(name.toString());
                file->setUrl(url.toString());
                file->setSkipHash(skipHash);
                file->setExists(true);
                m_files.emplaceBack(file);
            }

            loadCurrentSource(generation);
        }
    );
}
