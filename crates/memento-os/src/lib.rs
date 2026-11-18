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
//! idempotence state and the D-Bus cookie.

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
    fn apply_inhibit_reply(&mut self, reply: Option<u32>) {
        if let Some(cookie) = reply {
            if cookie != 0 {
                self.cookie = cookie;
                self.inhibited = true;
            }
        }
    }

    pub fn inhibit(&mut self, reason: &str) {
        if self.inhibited {
            return;
        }
        let reply = session_connection()
            .and_then(screen_saver_proxy)
            .and_then(|proxy| {
                proxy
                    .call::<_, _, u32>("Inhibit", &(APPLICATION_ID, reason))
                    .ok()
            });
        self.apply_inhibit_reply(reply);
    }

    pub fn uninhibit(&mut self) {
        if !self.inhibited {
            return;
        }
        if self.cookie != 0 {
            if let Some(connection) = session_connection() {
                if let Some(proxy) = screen_saver_proxy(connection) {
                    let _: zbus::Result<()> = proxy.call("UnInhibit", &(self.cookie,));
                }
            }
            self.cookie = 0;
        }
        self.inhibited = false;
    }
}

impl Drop for Screensaver {
    fn drop(&mut self) {
        self.uninhibit();
    }
}

#[cfg(test)]
mod tests {
    use super::Screensaver;

    #[test]
    fn inhibit_stays_false_on_failed_or_zero_cookie() {
        let mut s = Screensaver::default();
        s.apply_inhibit_reply(None);
        assert!(!s.inhibited);
        assert_eq!(s.cookie, 0);

        s.apply_inhibit_reply(Some(0));
        assert!(!s.inhibited);
        assert_eq!(s.cookie, 0);

        s.apply_inhibit_reply(Some(17));
        assert!(s.inhibited);
        assert_eq!(s.cookie, 17);

        s.inhibited = false;
        s.cookie = 0;
    }
}

