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

#include <algorithm>
#include <cmath>
#include <utility>
#include <vector>

#include <QItemSelectionModel>
#include <QtConcurrent>

#ifdef MEMENTO_SYSTEM_QCORO
#include <QCoroFuture>
#else
#include <qcoro/core/qcorofuture.h>
#endif

#include "player/mpvplayer.h"
#include "player/mpvstate.h"
#include "player/mpvsubtitle.h"
#include "player/mpvtrack.h"
#include "state/context.h"
#include "subtitle/subtitleentry.h"
#include "subtitle/subtitlelistmodel.h"
#include "subtitle/subtitlelists.h"
#include "subtitle/subtitleparser.h"

SubtitleListManager::SubtitleListManager(
    Context *context, QObject *parent) :
    QObject(parent),
    m_context(context),
    m_subtitleParser(std::make_unique<SubtitleParser>())
{
    connect(
        m_context->player()->state(), &MpvState::subtitleTracksChanged,
        this, &SubtitleListManager::handleSubtitleTracksChanged
    );
    connect(
        m_context->player(), &MpvPlayer::fileStarted,
        this, &SubtitleListManager::clearLists
    );
    connect(
        m_context->player(), &MpvPlayer::primarySubtitleLinesChanged,
        this, &SubtitleListManager::handlePrimarySubtitleLinesChanged
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
        m_context->player()->state()->subtitle(), &MpvSubtitle::speedChanged,
        this, &SubtitleListManager::handlePrimarySubtitleChanged
    );
    connect(
        m_context->player()->state()->secondarySubtitle(),
        &MpvSubtitle::speedChanged,
        this, &SubtitleListManager::handleSecondarySubtitleChanged
    );
    connect(
        m_context->player()->state()->subtitle(), &MpvSubtitle::delayChanged,
        this, &SubtitleListManager::handlePrimarySubtitleChanged
    );
    connect(
        m_context->player()->state()->secondarySubtitle(),
        &MpvSubtitle::textChanged,
        this,
        &SubtitleListManager::handleSecondarySubtitleChanged
    );
    connect(
        m_context->player()->state()->secondarySubtitle(),
        &MpvSubtitle::delayChanged,
        this,
        &SubtitleListManager::handleSecondarySubtitleChanged
    );
}

SubtitleListManager::~SubtitleListManager()
{

}

void SubtitleListManager::clearLists()
{
    SubtitleLists *lists = m_context->subtitleLists();
    m_memoryTrackIds.clear();
    m_externalPaths.clear();
    lists->setPrimary(nullptr);
    lists->setSecondary(nullptr);
    qDeleteAll(m_models);
    m_models.clear();
}

void SubtitleListManager::handleSubtitleTracksChanged()
{
    SubtitleLists *lists = m_context->subtitleLists();
    QSet<qint64> trackIds;
    const QList<MpvTrack *> &tracks =
        m_context->player()->state()->subtitleTracks();
    for (const MpvTrack *track : tracks)
    {
        if (track == nullptr || track->id() <= 0)
        {
            continue;
        }

        const qint64 trackId = track->id();
        trackIds.insert(trackId);
        SubtitleListModel *model = m_models.value(trackId, nullptr);
        if (model == nullptr)
        {
            model = new SubtitleListModel(m_context, this);
            m_models.insert(trackId, model);
        }

        if (track->external())
        {
            const QString path = track->externalFilename();
            if (!m_externalPaths.contains(trackId) ||
                m_externalPaths.value(trackId) != path)
            {
                model->clear();
                m_memoryTrackIds.remove(trackId);
                m_externalPaths.insert(trackId, path);
                if (!path.isEmpty())
                {
                    readExternalSubtitles(model, path);
                }
            }
        }
        else if (m_externalPaths.remove(trackId) > 0)
        {
            model->clear();
            m_memoryTrackIds.remove(trackId);
        }
    }

    for (auto modelIt = m_models.begin(); modelIt != m_models.end();)
    {
        if (trackIds.contains(modelIt.key()))
        {
            ++modelIt;
            continue;
        }

        const qint64 trackId = modelIt.key();
        SubtitleListModel *model = modelIt.value();
        if (lists->primary() == model)
        {
            lists->setPrimary(nullptr);
        }
        if (lists->secondary() == model)
        {
            lists->setSecondary(nullptr);
        }
        m_externalPaths.remove(trackId);
        m_memoryTrackIds.remove(trackId);
        delete model;
        modelIt = m_models.erase(modelIt);
    }

    handleSidChanged(m_context->player()->state()->sid());
    handleSecondarySidChanged(m_context->player()->state()->secondarySid());
}

void SubtitleListManager::handleSidChanged(int64_t sid)
{
    m_context->subtitleLists()->setPrimary(
        sid == 0 ? nullptr : m_models.value(sid, nullptr)
    );
}

void SubtitleListManager::handleSecondarySidChanged(int64_t sid)
{
    m_context->subtitleLists()->setSecondary(
        sid == 0 ? nullptr : m_models.value(sid, nullptr)
    );
}

void SubtitleListManager::handlePrimarySubtitleChanged()
{
    addSubtitle(
        m_context->subtitleLists()->primary(),
        m_context->player()->state()->subtitle(),
        m_context->player()->state()->timePosition()
    );
}

void SubtitleListManager::handleSecondarySubtitleChanged()
{
    addSubtitle(
        m_context->subtitleLists()->secondary(),
        m_context->player()->state()->secondarySubtitle(),
        m_context->player()->state()->timePosition()
    );
}

void SubtitleListManager::handlePrimarySubtitleLinesChanged(
    const QVariantList &lines)
{
    MpvPlayer *player = m_context->player();
    if (!player->isFileLoaded())
    {
        return;
    }

    MpvState *state = m_context->player()->state();
    const qint64 trackId = state->sid();
    SubtitleListModel *model = m_models.value(trackId, nullptr);
    if (trackId <= 0 || model == nullptr)
    {
        return;
    }

    if (!lines.isEmpty())
    {
        m_memoryTrackIds.insert(trackId);
    }
    if (!m_memoryTrackIds.contains(trackId))
    {
        return;
    }

    std::vector<SubtitleEntry> items;
    items.reserve(lines.size());
    for (const QVariant &value : lines)
    {
        const QVariantMap line = value.toMap();
        bool idValid = false;
        bool startValid = false;
        bool endValid = false;
        const qint64 cueId = line.value("id").toLongLong(&idValid);
        const double start = line.value("start").toDouble(&startValid);
        const double end = line.value("end").toDouble(&endValid);
        if (!idValid || cueId <= 0 || !startValid || !endValid ||
            !std::isfinite(start) || !std::isfinite(end) || start < 0.0 ||
            end <= start)
        {
            continue;
        }

        items.push_back({
            .text = line.value("text").toString(),
            .start = start,
            .end = end,
        });
    }
    std::stable_sort(
        items.begin(),
        items.end(),
        [] (const SubtitleEntry &lhs, const SubtitleEntry &rhs)
        {
            return lhs.start < rhs.start;
        }
    );

    model->setItems(std::move(items));
    model->selectPosition(
        state->subtitle()->rawTime(state->timePosition()),
        false
    );
}

void SubtitleListManager::addSubtitle(
    SubtitleListModel *model,
    MpvSubtitle *subtitle,
    double position)
{
    if (model == nullptr || subtitle == nullptr)
    {
        return;
    }

    if (!subtitle->text().isEmpty())
    {
        model->addSubtitle(
            subtitle->text(), subtitle->startTime(), subtitle->endTime());
        model->selectPosition(subtitle->rawTime(position));
    }
    else
    {
        model->selectionModel()->clear();
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
