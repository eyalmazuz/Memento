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

#include "subtitle/subtitlestate.h"

#include <utility>

SubtitleState::SubtitleState(QObject *parent) : QObject(parent)
{

}

const QString &SubtitleState::text() const noexcept
{
    return m_text;
}

double SubtitleState::startTime() const noexcept
{
    return m_startTime;
}

double SubtitleState::endTime() const noexcept
{
    return m_endTime;
}

double SubtitleState::delay() const noexcept
{
    return m_delay;
}

void SubtitleState::setSubtitle(
    QString text,
    double startTime,
    double endTime,
    double delay)
{
    bool dirty = false;
    if (m_text != text)
    {
        m_text = std::move(text);
        dirty = true;
        emit textChanged(m_text);
    }
    if (m_startTime != startTime)
    {
        m_startTime = startTime;
        dirty = true;
        emit startTimeChanged(m_startTime);
    }
    if (m_endTime != endTime)
    {
        m_endTime = endTime;
        dirty = true;
        emit endTimeChanged(m_endTime);
    }
    if (m_delay != delay)
    {
        m_delay = delay;
        dirty = true;
        emit delayChanged(m_delay);
    }
    if (dirty)
    {
        emit changed();
    }
}

void SubtitleState::clear()
{
    setSubtitle({}, 0.0, 0.0, 0.0);
}
