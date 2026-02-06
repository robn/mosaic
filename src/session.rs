use crate::geom::*;

use log::{debug, warn};
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use xcb::{x, Xid};

xcb::atoms_struct! {
    #[derive(Copy, Clone, Debug)]
    pub struct Atoms {
        wm_state => b"WM_STATE",

        net_wm_window_type => b"_NET_WM_WINDOW_TYPE",
        net_wm_window_type_normal => b"_NET_WM_WINDOW_TYPE_NORMAL",
        net_wm_window_type_dock => b"_NET_WM_WINDOW_TYPE_DOCK",
        net_wm_window_type_desktop => b"_NET_WM_WINDOW_TYPE_DESKTOP",

        net_active_window => b"_NET_ACTIVE_WINDOW",

        net_frame_extents => b"_NET_FRAME_EXTENTS",
        gtk_frame_extents => b"_GTK_FRAME_EXTENTS",

        pub net_moveresize_window => b"_NET_MOVERESIZE_WINDOW",

        net_wm_name => b"_NET_WM_NAME",
    }
}

// Session is sort of the entire X11 session at a moment in time. Not _exactly_ because the
// connection is live, but sort of conceptually what you expect.
//
// really, its a refcounted wrapper over SessionImpl, a sort of "internal" top object, so we can
// hand out references to it to let eg window ops feel natural
pub struct Session(Rc<SessionImpl>);

struct SessionImpl {
    conn: xcb::Connection,
    atoms: Atoms,
    root: x::Window,
    wg: OnceCell<WindowGroup>,
}

// WindowGroup is the snapshot of all the windows and any interesting categories or relationships.
// its conceptually part of Session/SessionImpl, but held separately so it can be lazily
// constructed and (in the future) refreshed
#[derive(Debug, Default)]
pub struct WindowGroup {
    windows: BTreeMap<u32, Window>,
    desktop: BTreeSet<u32>,
    dock: BTreeSet<u32>,
}

// Window represents a wraps a single X11 window. It has a reference to the session it came from so
// that it can call back into it for more advanced calls that require additional data from the
// server (eg extents) or state from other windows (eg absolute position)
#[derive(Debug)]
pub struct Window {
    sess: Session,
    pub id: u32,
    pub parent: u32,
    pub children: Vec<u32>,
    pub xw: x::Window,
    pub geom: Rect,
    pub typ: WindowType,
    pub selectable: bool,
}
#[derive(Debug)]
pub enum WindowType {
    Normal,
    Dock,
    Desktop,
    Root,
}

impl Session {
    pub(crate) fn init() -> xcb::Result<Session> {
        let (conn, scr_num) = xcb::Connection::connect(None)?;

        let atoms = Atoms::intern_all(&conn)?;

        let root = conn
            .get_setup()
            .roots()
            .nth(scr_num as usize)
            .unwrap()
            .to_owned()
            .root();

        Ok(Session(Rc::new(SessionImpl {
            conn,
            atoms,
            root,
            wg: OnceCell::new(),
        })))
    }

    pub(crate) fn window(&self, id: u32) -> &Window {
        &self.window_group().windows[&id]
    }
    pub(crate) fn root(&self) -> &Window {
        self.window(self.0.root.resource_id())
    }

    pub(crate) fn desktops(&self) -> impl Iterator<Item = &u32> {
        self.window_group().desktop.iter()
    }
    pub(crate) fn docks(&self) -> impl Iterator<Item = &u32> {
        self.window_group().dock.iter()
    }

    fn window_group(&self) -> &WindowGroup {
        self.0.wg.get_or_init(|| {
            let mut wg = WindowGroup::default();

            struct WindowCookies {
                xw: x::Window,
                parent: u32,
                geom: x::GetGeometryCookie,
                state_prop: x::GetPropertyCookie,
                type_prop: x::GetPropertyCookie,
            }

            fn get_window_state(
                sess: &Session,
                xw: x::Window,
                parent: u32,
            ) -> Vec<(WindowCookies, Vec<u32>)> {
                let tree_cookie = sess.x_query_tree(xw);

                let cookies = WindowCookies {
                    xw,
                    parent,
                    geom: sess.x_get_geometry(xw),
                    state_prop: sess.x_get_property(xw, sess.0.atoms.wm_state, x::ATOM_ANY),
                    type_prop: sess.x_get_property(
                        xw,
                        sess.0.atoms.net_wm_window_type,
                        x::ATOM_ANY,
                    ),
                };

                match sess.0.conn.wait_for_reply(tree_cookie) {
                    Ok(tree) => {
                        let parent = xw.resource_id();
                        let children = tree
                            .children()
                            .iter()
                            .map(|&cxw| cxw.resource_id())
                            .collect();

                        std::iter::once((cookies, children))
                            .chain(
                                tree.children()
                                    .iter()
                                    .map(|&cxw| get_window_state(sess, cxw, parent))
                                    .into_iter()
                                    .flatten(),
                            )
                            .collect()
                    }
                    Err(e) => {
                        warn!("QueryTree for window {:?} failed: {}", xw, e);
                        vec![(cookies, vec![])]
                    }
                }
            }

            for (wc, children) in get_window_state(self, self.0.root, self.0.root.resource_id()) {
                let geom = self.0.conn.wait_for_reply(wc.geom);
                let state_prop = self.0.conn.wait_for_reply(wc.state_prop);
                let type_prop = self.0.conn.wait_for_reply(wc.type_prop);
                match (geom, state_prop, type_prop) {
                    (Err(e), _, _) => warn!("GetGeometry for window {:?} failed: {}", wc.xw, e),
                    (_, Err(e), _) => {
                        warn!("GetProperty(WM_STATE) for window {:?} failed: {}", wc.xw, e)
                    }
                    (_, _, Err(e)) => warn!(
                        "GetProperty(NET_WM_WINDOW_TYPE) for window {:?} failed: {}",
                        wc.xw, e
                    ),
                    (Ok(geom), Ok(state_prop), Ok(type_prop)) => {
                        let id = wc.xw.resource_id();

                        let w = Window {
                            sess: Session(self.0.clone()),
                            id,
                            parent: wc.parent,
                            children,
                            xw: wc.xw,
                            geom: Rect::new(
                                (geom.x(), geom.y()).into(),
                                (geom.width() as i16, geom.height() as i16).into(),
                            ),
                            typ: match wc.xw == self.0.root {
                                true => WindowType::Root,
                                false => match type_prop.length() {
                                    // some clients (Spotify) do not set a _NET_WM_WINDOW_TYPE at all.
                                    // we already. we just treat them as TYPE_NORMAL here, because
                                    // unless they've been selected somehow it won't even matter.
                                    0 => WindowType::Normal,
                                    _ => match type_prop.value::<x::Atom>()[0] {
                                        v if v == self.0.atoms.net_wm_window_type_dock => {
                                            WindowType::Dock
                                        }
                                        v if v == self.0.atoms.net_wm_window_type_desktop => {
                                            WindowType::Desktop
                                        }
                                        _ => WindowType::Normal,
                                    },
                                },
                            },
                            //
                            // ICCCM mandates client root windows have WM_STATE, and we are only
                            // interested in NormalState (1)
                            selectable: state_prop.r#type() == self.0.atoms.wm_state
                                && state_prop.value::<u32>()[0] == 1,
                        };

                        match w.typ {
                            WindowType::Dock => {
                                wg.dock.insert(id);
                                ()
                            }
                            WindowType::Desktop => {
                                wg.desktop.insert(id);
                                ()
                            }
                            _ => {}
                        };

                        wg.windows.insert(id, w);
                    }
                }
            }

            wg
        })
    }

    pub(crate) fn active_window(&self) -> xcb::Result<&Window> {
        let active_prop = self.0.conn.wait_for_reply(self.x_get_property(
            self.0.root,
            self.0.atoms.net_active_window,
            x::ATOM_WINDOW,
        ))?;
        let id = active_prop.value()[0];
        Ok(self.window(id))
    }

    pub(crate) fn select_window(&self) -> xcb::Result<&Window> {
        let font = self.0.conn.generate_id();
        self.0.conn.send_request(&x::OpenFont {
            fid: font,
            name: b"cursor",
        });

        let cursor = self.0.conn.generate_id();
        self.0.conn.send_request(&x::CreateGlyphCursor {
            cid: cursor,
            source_font: font,
            mask_font: font,
            source_char: 34, // XC_crosshair
            mask_char: 35,
            fore_red: 0x0000,
            fore_green: 0x0000,
            fore_blue: 0x0000,
            back_red: 0xffff,
            back_green: 0xffff,
            back_blue: 0xffff,
        });

        self.0
            .conn
            .wait_for_reply(self.0.conn.send_request(&x::GrabPointer {
                owner_events: false,
                grab_window: self.0.root,
                event_mask: x::EventMask::BUTTON_PRESS | x::EventMask::BUTTON_RELEASE,
                pointer_mode: x::GrabMode::Sync,
                keyboard_mode: x::GrabMode::Async,
                confine_to: self.0.root,
                cursor: cursor,
                time: x::CURRENT_TIME,
            }))?;

        let selected = loop {
            self.0.conn.send_request(&x::AllowEvents {
                mode: x::Allow::SyncPointer,
                time: x::CURRENT_TIME,
            });
            self.0.conn.flush()?;

            if let xcb::Event::X(x::Event::ButtonPress(ev)) = self.0.conn.wait_for_event()? {
                let w = ev.child();
                if !w.is_none() {
                    break w;
                }
            }
        };

        self.0.conn.send_request(&x::UngrabPointer {
            time: x::CURRENT_TIME,
        });
        self.0.conn.flush()?;

        Ok(self.window(selected.resource_id()))
    }

    // legacy accessors
    pub(crate) fn conn(&self) -> &xcb::Connection {
        &self.0.conn
    }
    pub(crate) fn atoms(&self) -> &Atoms {
        &self.0.atoms
    }

    fn x_query_tree(&self, xw: x::Window) -> x::QueryTreeCookie {
        self.0.conn.send_request(&x::QueryTree { window: xw })
    }

    fn x_get_geometry(&self, xw: x::Window) -> x::GetGeometryCookie {
        self.0.conn.send_request(&x::GetGeometry {
            drawable: x::Drawable::Window(xw),
        })
    }

    fn x_get_property(&self, xw: x::Window, prop: x::Atom, ty: x::Atom) -> x::GetPropertyCookie {
        self.0.conn.send_request(&x::GetProperty {
            window: xw,
            delete: false,
            property: prop,
            r#type: ty,
            long_offset: 0,
            long_length: 512,
        })
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

impl Window {
    pub(crate) fn abs_xlate(&self) -> Vector2D {
        let mut id = self.id;
        let mut geom = self.geom;
        while id != self.sess.0.root.resource_id() {
            id = self.sess.window(id).parent;
            geom = geom.translate(self.sess.window(id).geom.origin.to_vector());
        }
        geom.min() - self.geom.min()
    }

    pub(crate) fn frame_extents(&self) -> xcb::Result<SideOffsets2D> {
        // XXX include gtk_frame_extents?

        let prop = self.sess.0.atoms.net_frame_extents;

        let extents_prop = self.sess.0.conn.wait_for_reply(self.sess.x_get_property(
            self.xw,
            prop,
            x::ATOM_CARDINAL,
        ))?;

        match extents_prop.r#type() {
            x::ATOM_CARDINAL => {
                let v: &[u32] = extents_prop.value();
                // CSS order: top, right, bottom, left
                // Cardinal order: left, right, bottom, top
                Ok(SideOffsets2D::new(
                    v[2] as i16,
                    v[1] as i16,
                    v[3] as i16,
                    v[0] as i16,
                ))
            }
            _ => {
                debug!(
                    "window {} has no extents {:?}, assuming zero",
                    self.id, prop
                );
                Ok(SideOffsets2D::zero())
            }
        }
    }

    pub(crate) fn _name(&self) -> xcb::Result<String> {
        // XXX some lazy cache for properties would be better
        let name_prop = self.sess.0.conn.wait_for_reply(self.sess.x_get_property(
            self.xw,
            self.sess.0.atoms.net_wm_name,
            x::ATOM_ANY,
        ))?;
        Ok(String::from_utf8_lossy(name_prop.value()).to_string())
    }
}
