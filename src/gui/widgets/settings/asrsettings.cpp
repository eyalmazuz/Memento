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

#include "asrsettings.h"
#include "ui_asrsettings.h"


/* Begin Constructor/Destructor */

ASRSettings::ASRSettings(QWidget *parent)
    : QWidget(parent),
      m_ui(new Ui::ASRSettings)
{
    m_ui->setupUi(this);

}

ASRSettings::~ASRSettings()
{
    disconnect();
    delete m_ui;
}

/* End Constructor/Destructor */
/* Begin Event Handlers */

void ASRSettings::showEvent(QShowEvent *event)
{

}

/* End Event Handlers */
/* Begin Button Box Handlers */

void ASRSettings::restoreSaved()
{

}

void ASRSettings::restoreDefaults()
{

}

void ASRSettings::applySettings()
{

}

/* End Button Box Handlers */
/* Begin Open File Handlers */

void ASRSettings::setLocalModel()
{

}

/* End Open File Handlers */
