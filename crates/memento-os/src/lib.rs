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

//! Linux session-bus screensaver inhibition. The handle owns both the
//! idempotence state and the D-Bus cookie; the Qt object only forwards UTF-8.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    OnceLock,
};

use zbus::blocking::{fdo::DBusProxy, Connection, Proxy};
use zbus::names::BusName;
use zbus_xml::Node;

const SERVICE: &str = "org.freedesktop.ScreenSaver";
const PATH: &str = "/org/freedesktop/ScreenSaver";
const INTERFACE: &str = "org.freedesktop.ScreenSaver";
const APPLICATION_ID: &str = "io.github.ripose-jp.memento";

// Keep a single session-bus sender for the whole application, as Qt does.
// Many ScreenSaver services associate a cookie with its D-Bus sender and
// automatically release it if that sender disconnects between the two calls.
static SESSION: OnceLock<Connection> = OnceLock::new();

fn session_connection() -> Option<&'static Connection> {
    if let Some(connection) = SESSION.get() {
        return Some(connection);
    }
    // Failure is not cached: a session bus can become available later.
    let connection = Connection::session().ok()?;
    Some(SESSION.get_or_init(|| connection))
}

// QDBusInterface introspects before looking up the service owner and caches
// metadata after it sees this interface. The first lookup may activate a
// D-Bus-activatable service; later lookups only need the owner check.
static METADATA_CACHED: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
pub struct Screensaver {
    inhibited: bool,
    cookie: u32,
}

// Qt's QDBusInterface::isValid() checks whether the service currently owns
// its bus name, NOT whether the requested interface appears in introspection.
// When another interface is exported at the path, Qt still attempts the call,
// and UnInhibit clears the cookie even if the call fails.
fn screen_saver_proxy(connection: &Connection) -> Option<Proxy<'_>> {
    let proxy = Proxy::new(connection, SERVICE, PATH, INTERFACE).ok()?;
    if !METADATA_CACHED.load(Ordering::Relaxed) {
        if let Ok(xml) = proxy.introspect() {
            if let Ok(node) = Node::try_from(xml.as_str()) {
                if node
                    .interfaces()
                    .iter()
                    .any(|interface| interface.name().as_str() == INTERFACE)
                {
                    METADATA_CACHED.store(true, Ordering::Relaxed);
                }
            }
        }
    }
    let bus = DBusProxy::new(connection).ok()?;
    if !bus.name_has_owner(BusName::try_from(SERVICE).ok()?).ok()? {
        return None;
    }
    Some(proxy)
}

impl Screensaver {
    pub fn inhibit(&mut self, reason: &str) {
        if self.inhibited {
            return;
        }
        if let Some(connection) = session_connection() {
            if let Some(proxy) = screen_saver_proxy(connection) {
                if let Ok(cookie) = proxy.call("Inhibit", &(APPLICATION_ID, reason)) {
                    self.cookie = cookie;
                }
            }
        }
        // The flag is set even if opening the bus, finding the interface, or
        // calling Inhibit failed. The cookie is unchanged on failed calls.
        self.inhibited = true;
    }

    pub fn uninhibit(&mut self) {
        if !self.inhibited {
            return;
        }
        if self.cookie != 0 {
            if let Some(connection) = session_connection() {
                if let Some(proxy) = screen_saver_proxy(connection) {
                    let _: zbus::Result<()> = proxy.call("UnInhibit", &(self.cookie,));
                    // Qt clears the cookie when the interface is valid,
                    // regardless of the method reply (but not when absent).
                    self.cookie = 0;
                }
            }
        }
        self.inhibited = false;
    }
}

impl Drop for Screensaver {
    fn drop(&mut self) {
        self.uninhibit();
    }
}

/// The C++ shells call these through `src/os/rust/screensaver_capi.h`.
///
/// Mirrors the QDBus C-string marshaling semantics: `reason` bytes are read
/// up to the first NUL, exactly like `QByteArray::constData()` fed to a
/// D-Bus `"s"` argument.
#[no_mangle]
pub extern "C" fn memento_os_screensaver_new() -> *mut Screensaver {
    Box::into_raw(Box::new(Screensaver::default()))
}

/// # Safety
/// `handle` must be null or a live handle returned by `memento_os_screensaver_new`,
/// passed exactly once. No concurrent calls on the same handle are permitted.
#[no_mangle]
pub unsafe extern "C" fn memento_os_screensaver_free(handle: *mut Screensaver) {
    if !handle.is_null() {
        drop(Box::from_raw(handle));
    }
}

/// # Safety
/// `handle` must be a live exclusive handle (or null); `reason` must point to
/// `reason_len` readable bytes while this function runs, or be null if empty.
/// The bytes are borrowed, never retained. `QString::toUtf8()` supplies valid UTF-8.
#[no_mangle]
pub unsafe extern "C" fn memento_os_screensaver_inhibit(
    handle: *mut Screensaver,
    reason: *const u8,
    reason_len: usize,
) {
    let Some(handle) = handle.as_mut() else {
        return;
    };
    if reason_len != 0 && reason.is_null() {
        return;
    }
    let bytes = if reason_len == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(reason, reason_len)
    };
    // QDBus marshals `const char *` arguments as C strings: truncate at the
    // first NUL byte. Rejects ill-formed UTF-8 defensively; QString::toUtf8
    // replaces unpaired surrogates, so production inputs are always valid.
    if let Some(reason_str) = core::str::from_utf8(
        bytes.split(|&byte| byte == 0).next().unwrap_or_default(),
    )
    .ok()
    {
        handle.inhibit(reason_str);
    }
}

/// # Safety
/// `handle` must be a live exclusive handle (or null).
#[no_mangle]
pub unsafe extern "C" fn memento_os_screensaver_uninhibit(handle: *mut Screensaver) {
    if let Some(handle) = handle.as_mut() {
        handle.uninhibit();
    }
}

