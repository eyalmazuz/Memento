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

#include "manager/subtitlelistmanager.h"

#include <QPointer>
#include <QtConcurrentRun>

#ifdef MEMENTO_SYSTEM_QCORO
#include <QCoroFuture>
#else
#include <qcoro/core/qcorofuture.h>
#endif // MEMENTO_SYSTEM_QCORO

#include "player/mpvplayer.h"
#include "state/context.h"
#include "subtitle/subtitleparser.h"
#include "subtitle/subtitlestate.h"

SubtitleListManager::SubtitleListManager(Context *context, QObject *parent) :
    QObject(parent),
    m_context(context),
    m_subtitleParser(std::make_unique<SubtitleParser>())
{
    connect(
        m_context->player()->state(), &MpvState::subtitleTracksChanged,
        this, &SubtitleListManager::handleSubtitleTracksChanged
    );
    connect(
        m_context->player()->state(), &MpvState::sidChanged,
        this, &SubtitleListManager::handleSidChanged
    );
    connect(
        m_context->player()->state(), &MpvState::secondarySidChanged,
        this, &SubtitleListManager::handleSecondarySidChanged
    );
    connect(
        m_context->player()->state()->subtitle(), &MpvSubtitle::textChanged,
        this, &SubtitleListManager::handlePrimarySubtitleChanged
    );
    connect(
        m_context->player()->state()->secondarySubtitle(),
        &MpvSubtitle::textChanged,
        this,
        &SubtitleListManager::handleSecondarySubtitleChanged
    );
}

SubtitleListManager::~SubtitleListManager()
{

}

void SubtitleListManager::clearLists()
{
    if (m_context->subtitleLists()->primarySource() !=
        SubtitleLists::Internal)
    {
        m_context->subtitleLists()->setPrimary(nullptr);
    }
    if (m_context->subtitleLists()->secondarySource() !=
        SubtitleLists::Internal)
    {
        m_context->subtitleLists()->setSecondary(nullptr);
    }
    qDeleteAll(m_models);
    m_models.clear();
}

void SubtitleListManager::handleSubtitleTracksChanged()
{
    clearLists();

    const QList<MpvTrack *> &tracks =
        m_context->player()->state()->subtitleTracks();
    for (const MpvTrack *track : tracks)
    {
        m_models.emplaceBack(new SubtitleListModel(m_context, this));
        if (track->external())
        {
            readExternalSubtitles(m_models.back(), track->externalFilename());
        }
    }
    handleSidChanged(m_context->player()->state()->sid());
    handleSecondarySidChanged(m_context->player()->state()->secondarySid());
}

void SubtitleListManager::handleSidChanged(int64_t sid)
{
    if (m_context->subtitleLists()->primarySource() ==
        SubtitleLists::Internal)
    {
        return;
    }

    if (sid > m_models.size())
    {
        return;
    }
    m_context->subtitleLists()->setPrimary(
        sid == 0 ? nullptr : m_models[sid - 1]
    );
}

void SubtitleListManager::handleSecondarySidChanged(int64_t sid)
{
    if (m_context->subtitleLists()->secondarySource() ==
        SubtitleLists::Internal)
    {
        return;
    }

    if (sid > m_models.size())
    {
        return;
    }
    m_context->subtitleLists()->setSecondary(
        sid == 0 ? nullptr : m_models[sid - 1]
    );
}

void SubtitleListManager::handlePrimarySubtitleChanged()
{
    if (m_context->subtitleLists()->primarySource() ==
        SubtitleLists::Internal)
    {
        return;
    }

    addSubtitle(
        m_context->subtitleLists()->primary(),
        m_context->subtitleLists()->primaryState(),
        m_context->player()->state()->subtitle(),
        m_context->player()->state()->timePosition()
    );
}

void SubtitleListManager::handleSecondarySubtitleChanged()
{
    if (m_context->subtitleLists()->secondarySource() ==
        SubtitleLists::Internal)
    {
        return;
    }

    addSubtitle(
        m_context->subtitleLists()->secondary(),
        m_context->subtitleLists()->secondaryState(),
        m_context->player()->state()->secondarySubtitle(),
        m_context->player()->state()->timePosition()
    );
}

void SubtitleListManager::addSubtitle(
    SubtitleListModel *model,
    SubtitleState *state,
    MpvSubtitle *subtitle,
    double position)
{
    if (state == nullptr || subtitle == nullptr)
    {
        return;
    }

    if (!subtitle->text().isEmpty())
    {
        if (model != nullptr)
        {
            model->addSubtitle(
                subtitle->text(), subtitle->startTime(), subtitle->endTime());
            model->selectPosition(position - subtitle->delay());
        }
        state->setSubtitle(
            subtitle->text(),
            subtitle->startTime(),
            subtitle->endTime(),
            subtitle->delay()
        );
    }
    else
    {
        if (model != nullptr)
        {
            model->selectionModel()->clear();
        }
        state->clear();
    }
}

QCoro::Task<void> SubtitleListManager::readExternalSubtitles(
    QPointer<SubtitleListModel> model, QString path)
{
    QFuture<std::vector<SubtitleEntry>> future = QtConcurrent::run(
        &SubtitleParser::parseSubtitles,
        m_subtitleParser.get(),
        path
    );

    std::vector<SubtitleEntry> items = co_await qCoro(future).takeResult();
    if (items.empty() || model == nullptr)
    {
        co_return;
    }
    model->setItems(std::move(items));
}
