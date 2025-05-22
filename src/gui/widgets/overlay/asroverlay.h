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

#ifndef ASROVERLAY_H
#define ASROVERLAY_H

#include <QWidget>

#include <QFutureWatcher>

#include "asr/asrmodel.h"

class QRubberBand;

/**
 * Widget for selecting an area of its parent and running ASR on it.
 */
class ASROverlay : public QWidget
{
    Q_OBJECT

public:
    /**
     * Construct a new ASROverlay widget.
     * @param parent The parent that will have ASR run it.
     */
     ASROverlay(QWidget *parent = nullptr);

    virtual ~ASROverlay();

// private Q_SLOTS:
//     /**
//      * Initializes settings with respect to settings.
//      */
//     void initASRSettings();

// Q_SIGNALS:
//     /**
//      * Emitted when an OCR search completes.
//      * @param text The returned text.
//      */
//     void finished(const QString &text) const;

//     /**
//      * Emitted when this widget is hidden.
//      */
//     void widgetHidden() const;

// protected:
//     /**
//      * Called when the mouse is pressed. Shows the OCR area.
//      * @param event The mouse press event.
//      */
//     void mousePressEvent(QMouseEvent *event) override;

//     /**
//      * Called when the mouse is moved. Resizes the OCR area.
//      * @param event The mouse move event.
//      */
//     void mouseMoveEvent(QMouseEvent *event) override;

//     /**
//      * Called when the mouse is released. Executes an OCR search.
//      * @param event The mouse release event.
//      */
//     void mouseReleaseEvent(QMouseEvent *event) override;

//     /**
//      * Called when this widget is hidden.
//      * @param event The hide event.
//      */
//     void hideEvent(QHideEvent *event) override;

// private Q_SLOTS:
//     /**
//      * Handles a keypress event. This is used to cancel the overlay on escape.
//      * @param event The keypress event.
//      */
//     void handleKeyPress(QKeyEvent *event);

// private:
//     /**
//      * Uses ASR to get text from a given area of its parent.
//      * @param rect The area of sample from the parent widget.
//      */
//     void getText(QRect rect);

    /* The ASR model */
    ASRModel *m_model = nullptr;

//     /* Watches the last QFuture returned from ASRModel */
//     QFutureWatcher<QString> m_resultWatcher;

    /* The QRubberBand widget that represents the selected area */
    QRubberBand *m_rubberBand;

//     /* The point where the mouse was first pressed in the last search */
//     QPoint m_startPoint;
};

#endif // ASROVERLAY_H
