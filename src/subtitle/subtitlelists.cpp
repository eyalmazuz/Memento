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

#include "subtitle/subtitlelists.h"

#include <utility>

SubtitleLists::SubtitleLists(QObject *parent) : QObject(parent)
{

}

SubtitleListModel *SubtitleLists::primary() const noexcept
{
    return m_primary;
}

void SubtitleLists::setPrimary(SubtitleListModel *value)
{
    setPrimary(value, defaultSource(value));
}

void SubtitleLists::setPrimary(SubtitleListModel *value, Source source)
{
    const bool modelChanged = m_primary != value;
    const bool sourceChanged = m_primarySource != source;
    if (modelChanged)
    {
        m_primary = value;
        emit primaryChanged(m_primary);
    }
    setPrimarySource(source);
    if (modelChanged || sourceChanged || source == None)
    {
        clearPrimarySubtitle();
    }
}

SubtitleListModel *SubtitleLists::secondary() const noexcept
{
    return m_secondary;
}

void SubtitleLists::setSecondary(SubtitleListModel *value)
{
    setSecondary(value, defaultSource(value));
}

void SubtitleLists::setSecondary(SubtitleListModel *value, Source source)
{
    const bool modelChanged = m_secondary != value;
    const bool sourceChanged = m_secondarySource != source;
    if (modelChanged)
    {
        m_secondary = value;
        emit secondaryChanged(m_secondary);
    }
    setSecondarySource(source);
    if (modelChanged || sourceChanged || source == None)
    {
        clearSecondarySubtitle();
    }
}

SubtitleState *SubtitleLists::primaryState() const noexcept
{
    return m_primaryState;
}

SubtitleState *SubtitleLists::secondaryState() const noexcept
{
    return m_secondaryState;
}

SubtitleLists::Source SubtitleLists::primarySource() const noexcept
{
    return m_primarySource;
}

SubtitleLists::Source SubtitleLists::secondarySource() const noexcept
{
    return m_secondarySource;
}

void SubtitleLists::setPrimarySubtitle(
    QString text,
    double startTime,
    double endTime,
    double delay)
{
    m_primaryState->setSubtitle(
        std::move(text),
        startTime,
        endTime,
        delay
    );
}

void SubtitleLists::setSecondarySubtitle(
    QString text,
    double startTime,
    double endTime,
    double delay)
{
    m_secondaryState->setSubtitle(
        std::move(text),
        startTime,
        endTime,
        delay
    );
}

void SubtitleLists::clearPrimarySubtitle()
{
    m_primaryState->clear();
}

void SubtitleLists::clearSecondarySubtitle()
{
    m_secondaryState->clear();
}

SubtitleLists::Source SubtitleLists::defaultSource(
    SubtitleListModel *value) const noexcept
{
    return value == nullptr ? None : Mpv;
}

void SubtitleLists::setPrimarySource(Source value)
{
    if (m_primarySource == value)
    {
        return;
    }
    m_primarySource = value;
    emit primarySourceChanged(m_primarySource);
}

void SubtitleLists::setSecondarySource(Source value)
{
    if (m_secondarySource == value)
    {
        return;
    }
    m_secondarySource = value;
    emit secondarySourceChanged(m_secondarySource);
}
