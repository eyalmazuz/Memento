//! QML-facing screensaver controller. Qt generates the QObject/meta-object;
//! the inhibition lifecycle and D-Bus connection belong to Rust.
//!
//! NOTE: Phase-B (cxx-qt) PROTOTYPE — intentionally outside `src/`, not
//! compiled by the crate (`build.rs` is a no-op). See build.rs for the
//! reason it was parked in this round.</think><tool_call>edit<arg_key>newString</arg_key><arg_value>//! NOTE: Phase-B (cxx-qt) PROTOTYPE — intentionally outside `src/`, not
//! compiled by the crate (`build.rs` is a no-op). See `build.rs` for why
//! it was parked this round.
//!
//! QML-facing screensaver controller. Qt generates the QObject/meta-object;
//! the inhibition lifecycle and D-Bus connection belong to Rust.

use core::pin::Pin;
use cxx_qt::CxxQtType;
use cxx_qt_lib::QString;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        #[namespace = "memento_os"]
        type ScreensaverQt = super::ScreensaverQtRust;

        #[qinvokable]
        fn inhibit(self: Pin<&mut ScreensaverQt>, reason: &QString);

        #[qinvokable]
        fn uninhibit(self: Pin<&mut ScreensaverQt>);
    }
}

#[derive(Default)]
pub struct ScreensaverQtRust {
    backend: crate::Screensaver,
}

impl qobject::ScreensaverQt {
    pub fn inhibit(mut self: Pin<&mut Self>, reason: &QString) {
        let utf8 = reason.to_utf8();
        let bytes = utf8.as_slice();
        // The former Qt QDBus call passed QByteArray::constData() as a C string.
        let c_string = bytes.split(|&byte| byte == 0).next().unwrap_or_default();
        if let Ok(reason) = core::str::from_utf8(c_string) {
            self.as_mut().rust_mut().backend.inhibit(reason);
        }
    }

    pub fn uninhibit(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().backend.uninhibit();
    }
}
