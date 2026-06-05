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

#include "subtitle/subtitlelistmodel.h"
#include "subtitle/subtitlestate.h"

/**
 * @brief Object holding the current lists for the subtitle tracks.
 */
class SubtitleLists : public QObject
{
    Q_OBJECT

    Q_PROPERTY(
        SubtitleListModel *primary
        READ primary
        NOTIFY primaryChanged
    )

    Q_PROPERTY(
        SubtitleListModel *secondary
        READ secondary
        NOTIFY secondaryChanged
    )

    Q_PROPERTY(
        SubtitleState *primaryState
        READ primaryState
        CONSTANT
    )

    Q_PROPERTY(
        SubtitleState *secondaryState
        READ secondaryState
        CONSTANT
    )

    Q_PROPERTY(
        Source primarySource
        READ primarySource
        NOTIFY primarySourceChanged
    )

    Q_PROPERTY(
        Source secondarySource
        READ secondarySource
        NOTIFY secondarySourceChanged
    )

public:
    /**
     * @brief The source that owns a subtitle list and current subtitle state.
     */
    enum Source
    {
        None = 0,
        Mpv,
        Internal,
    };
    Q_ENUM(Source)

    SubtitleLists(QObject *parent = nullptr);
    virtual ~SubtitleLists() = default;

    /**
     * @brief Get the primary subtitle list model.
     *
     * @return The primary subtitle list model, nullptr if no subtitle is
     * selected.
     */
    [[nodiscard]]
    SubtitleListModel *primary() const noexcept;

    /**
     * @brief Set the primary subtitle list model as an mpv source.
     *
     * @param value The primary subtitle list model.
     */
    void setPrimary(SubtitleListModel *value);

    /**
     * @brief Set the primary subtitle list model and source.
     *
     * @param value The primary subtitle list model.
     * @param source The source that owns the model.
     */
    void setPrimary(SubtitleListModel *value, Source source);

    /**
     * @brief Get the secondary subtitle list model.
     *
     * @return The secondary subtitle list model, nullptr if no subtitle is
     * selected.
     */
    [[nodiscard]]
    SubtitleListModel *secondary() const noexcept;

    /**
     * @brief Set the secondary subtitle list model as an mpv source.
     *
     * @param value The secondary subtitle list model.
     */
    void setSecondary(SubtitleListModel *value);

    /**
     * @brief Set the secondary subtitle list model and source.
     *
     * @param value The secondary subtitle list model.
     * @param source The source that owns the model.
     */
    void setSecondary(SubtitleListModel *value, Source source);

    /**
     * @brief Get the primary current subtitle state.
     *
     * @return The primary current subtitle state.
     */
    [[nodiscard]]
    SubtitleState *primaryState() const noexcept;

    /**
     * @brief Get the secondary current subtitle state.
     *
     * @return The secondary current subtitle state.
     */
    [[nodiscard]]
    SubtitleState *secondaryState() const noexcept;

    /**
     * @brief Get the source that owns the primary subtitle state.
     *
     * @return The primary subtitle source.
     */
    [[nodiscard]]
    Source primarySource() const noexcept;

    /**
     * @brief Get the source that owns the secondary subtitle state.
     *
     * @return The secondary subtitle source.
     */
    [[nodiscard]]
    Source secondarySource() const noexcept;

    /**
     * @brief Set the current primary subtitle.
     *
     * @param text The subtitle text.
     * @param startTime The subtitle start time.
     * @param endTime The subtitle end time.
     * @param delay The subtitle delay.
     */
    void setPrimarySubtitle(
        QString text,
        double startTime,
        double endTime,
        double delay = 0.0
    );

    /**
     * @brief Set the current secondary subtitle.
     *
     * @param text The subtitle text.
     * @param startTime The subtitle start time.
     * @param endTime The subtitle end time.
     * @param delay The subtitle delay.
     */
    void setSecondarySubtitle(
        QString text,
        double startTime,
        double endTime,
        double delay = 0.0
    );

    /**
     * @brief Clear the current primary subtitle.
     */
    void clearPrimarySubtitle();

    /**
     * @brief Clear the current secondary subtitle.
     */
    void clearSecondarySubtitle();

signals:
    /**
     * @brief Emitted when the primary subtitle list is changed.
     *
     * @param value The new subtitle list.
     */
    void primaryChanged(SubtitleListModel *value);

    /**
     * @brief Emitted when the secondary subtitle list is changed.
     *
     * @param value The new subtitle list.
     */
    void secondaryChanged(SubtitleListModel *value);

    /**
     * @brief Emitted when the primary subtitle source changes.
     *
     * @param value The new primary subtitle source.
     */
    void primarySourceChanged(Source value);

    /**
     * @brief Emitted when the secondary subtitle source changes.
     *
     * @param value The new secondary subtitle source.
     */
    void secondarySourceChanged(Source value);

private:
    /**
     * @brief Get the default source for a subtitle list model.
     *
     * @param value The subtitle list model.
     * @return Mpv if value exists, None otherwise.
     */
    [[nodiscard]]
    Source defaultSource(SubtitleListModel *value) const noexcept;

    /**
     * @brief Set the primary subtitle source.
     *
     * @param value The new source.
     */
    void setPrimarySource(Source value);

    /**
     * @brief Set the secondary subtitle source.
     *
     * @param value The new source.
     */
    void setSecondarySource(Source value);

    /* Pointer to the primary subtitle list. Does not have ownership. */
    SubtitleListModel *m_primary{nullptr};

    /* Pointer to the secondary subtitle list. Does not have ownership. */
    SubtitleListModel *m_secondary{nullptr};

    /* The current primary subtitle state */
    SubtitleState *m_primaryState{new SubtitleState(this)};

    /* The current secondary subtitle state */
    SubtitleState *m_secondaryState{new SubtitleState(this)};

    /* The source that owns primary and primaryState */
    Source m_primarySource{None};

    /* The source that owns secondary and secondaryState */
    Source m_secondarySource{None};
};
