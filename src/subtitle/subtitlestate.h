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

#include <QObject>
#include <QString>

/**
 * @brief Holds the current subtitle text and timing independent of its source.
 */
class SubtitleState : public QObject
{
    Q_OBJECT

    Q_PROPERTY(
        QString text
        READ text
        NOTIFY textChanged
    )

    Q_PROPERTY(
        double startTime
        READ startTime
        NOTIFY startTimeChanged
    )

    Q_PROPERTY(
        double endTime
        READ endTime
        NOTIFY endTimeChanged
    )

    Q_PROPERTY(
        double delay
        READ delay
        NOTIFY delayChanged
    )

public:
    explicit SubtitleState(QObject *parent = nullptr);
    virtual ~SubtitleState() = default;

    /**
     * @brief Get the current subtitle text.
     *
     * @return The current subtitle text.
     */
    [[nodiscard]]
    const QString &text() const noexcept;

    /**
     * @brief Get the current subtitle start time.
     *
     * @return The current subtitle start time in seconds.
     */
    [[nodiscard]]
    double startTime() const noexcept;

    /**
     * @brief Get the current subtitle end time.
     *
     * @return The current subtitle end time in seconds.
     */
    [[nodiscard]]
    double endTime() const noexcept;

    /**
     * @brief Get the current subtitle delay.
     *
     * @return The current subtitle delay in seconds.
     */
    [[nodiscard]]
    double delay() const noexcept;

    /**
     * @brief Set all current subtitle fields.
     *
     * @param text The subtitle text.
     * @param startTime The subtitle start time.
     * @param endTime The subtitle end time.
     * @param delay The subtitle delay.
     */
    void setSubtitle(
        QString text,
        double startTime,
        double endTime,
        double delay = 0.0
    );

    /**
     * @brief Clear the current subtitle.
     */
    void clear();

signals:
    /**
     * @brief Emitted when the current subtitle text changes.
     *
     * @param value The new subtitle text.
     */
    void textChanged(const QString &value);

    /**
     * @brief Emitted when the current subtitle start time changes.
     *
     * @param value The new start time.
     */
    void startTimeChanged(double value);

    /**
     * @brief Emitted when the current subtitle end time changes.
     *
     * @param value The new end time.
     */
    void endTimeChanged(double value);

    /**
     * @brief Emitted when the current subtitle delay changes.
     *
     * @param value The new delay.
     */
    void delayChanged(double value);

    /**
     * @brief Emitted once after any current subtitle field changes.
     */
    void changed();

private:
    /* The current subtitle text */
    QString m_text;

    /* The current subtitle start time */
    double m_startTime{0.0};

    /* The current subtitle end time */
    double m_endTime{0.0};

    /* The current subtitle delay */
    double m_delay{0.0};
};
