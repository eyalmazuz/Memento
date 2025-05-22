////////////////////////////////////////////////////////////////////////////////
//
// Copyright (c) 2022 Ripose
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

#include "asrmodel.h"

#include <QtConcurrent>

#include <whisper.h>

#ifdef Q_OS_DARWIN
#include <pthread.h>

#define APPLE_STACK_SIZE ((size_t)8388608)
#endif // Q_OS_DARWIN


ASRModel::ASRModel(const QString &model, bool useGPU)
{
    // m_model = nullptr;
}

ASRModel::~ASRModel()
 {
    //  QWriteLocker locker(&m_modelLock);
 }