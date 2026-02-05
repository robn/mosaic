use clap::{ArgGroup, Parser, ValueEnum};
use log::{debug, warn};
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use xcb::{x, Xid};

xcb::atoms_struct! {
    #[derive(Copy, Clone, Debug)]
    struct Atoms {
        wm_state => b"WM_STATE",

        net_wm_window_type => b"_NET_WM_WINDOW_TYPE",
        net_wm_window_type_normal => b"_NET_WM_WINDOW_TYPE_NORMAL",
        net_wm_window_type_dock => b"_NET_WM_WINDOW_TYPE_DOCK",
        net_wm_window_type_desktop => b"_NET_WM_WINDOW_TYPE_DESKTOP",

        net_active_window => b"_NET_ACTIVE_WINDOW",

        net_frame_extents => b"_NET_FRAME_EXTENTS",
        gtk_frame_extents => b"_GTK_FRAME_EXTENTS",

        net_moveresize_window => b"_NET_MOVERESIZE_WINDOW",

        net_wm_name => b"_NET_WM_NAME",
    }
}

// XXX use ArgGroup enums for target: https://github.com/clap-rs/clap/issues/2621
#[derive(Parser, Debug)]
#[clap(group(ArgGroup::new("target").required(true)))]
struct RootArgs {
    #[clap(long, group = "target", value_parser=clap_num::maybe_hex::<u32>)]
    id: Option<u32>,

    #[clap(long, group = "target")]
    active: bool,

    #[clap(long, group = "target")]
    select: bool,

    #[clap(long, value_enum)]
    horiz: HorizSpec,

    #[clap(long, value_enum)]
    vert: VertSpec,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum HorizSpec {
    Current,
    Left,
    Left25,
    Left50,
    Left75,
    Right,
    Right25,
    Right50,
    Right75,
    Full,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum VertSpec {
    Current,
    Top,
    Bottom,
    Full,
}

#[derive(Debug)]
enum TargetArgs {
    None,
    Id(u32),
    Select,
    Active,
}

struct RootSpace;
type Rect = euclid::Rect<i16, RootSpace>;
type Box2D = euclid::Box2D<i16, RootSpace>;
type Vector2D = euclid::Vector2D<i16, RootSpace>;
type SideOffsets2D = euclid::SideOffsets2D<i16, RootSpace>;

/*
#[derive(Clone, Copy, Debug)]
struct Bounds {
    x: i16,
    y: i16,
    w: i16,
    h: i16,
}

#[derive(Debug)]
struct Extents {
    left: i16,
    right: i16,
    top: i16,
    bottom: i16,
}
*/

bitflags::bitflags! {
    struct MoveResizeWindowFlags: u32 {
        const GRAVITY_IMPLIED    = 0;
        const GRAVITY_NORTH_WEST = 1;
        const GRAVITY_NORTH      = 2;
        const GRAVITY_NORTH_EAST = 3;
        const GRAVITY_WEST       = 4;
        const GRAVITY_CENTER     = 5;
        const GRAVITY_EAST       = 6;
        const GRAVITY_SOUTH_WEST = 7;
        const GRAVITY_SOUTH      = 8;
        const GRAVITY_SOUTH_EAST = 9;
        const GRAVITY_STATIC     = 10;
        const X                  = 1 << 8;
        const Y                  = 1 << 9;
        const WIDTH              = 1 << 10;
        const HEIGHT             = 1 << 11;
    }
}

#[derive(Debug)]
struct Window {
    id: u32,
    xw: x::Window,
    geom: Rect,
    typ: WindowType,
}

#[derive(Debug)]
enum WindowType {
    Normal,
    Dock,
    Desktop,
    Root,
}

struct Context {
    conn: xcb::Connection,
    atoms: Atoms,
    root: x::Window,
    wg: OnceCell<WindowGroup>,
}

#[derive(Debug, Default)]
struct WindowGroup {
    windows: BTreeMap<u32, Window>,
    root: Option<u32>,
    desktop: BTreeSet<u32>,
    dock: BTreeSet<u32>,
    selectable: BTreeSet<u32>,
    parents: BTreeMap<u32, u32>,
}

impl Context {
    fn new() -> xcb::Result<Context> {
        let (conn, scr_num) = xcb::Connection::connect(None)?;

        let atoms = Atoms::intern_all(&conn)?;

        let root = conn
            .get_setup()
            .roots()
            .nth(scr_num as usize)
            .unwrap()
            .to_owned()
            .root();

        Ok(Context {
            conn,
            atoms,
            root,
            wg: OnceCell::new(),
        })
    }

    fn window(&self, id: u32) -> &Window {
        &self.window_group().windows[&id]
    }

    fn window_group(&self) -> &WindowGroup {
        self.wg.get_or_init(|| {
            let mut wg = WindowGroup::default();

            struct WindowCookies {
                xw: x::Window,
                parent_id: u32,
                geom: x::GetGeometryCookie,
                state_prop: x::GetPropertyCookie,
                type_prop: x::GetPropertyCookie,
            }

            fn get_window_cookies(
                ctx: &Context,
                parent_id: u32,
                xw: x::Window,
            ) -> Vec<WindowCookies> {
                let tree_cookie = ctx.x_query_tree(xw);

                let cookies = WindowCookies {
                    xw,
                    parent_id,
                    geom: ctx.x_get_geometry(xw),
                    state_prop: ctx.x_get_property(xw, ctx.atoms.wm_state, x::ATOM_ANY),
                    type_prop: ctx.x_get_property(xw, ctx.atoms.net_wm_window_type, x::ATOM_ANY),
                };

                match ctx.conn.wait_for_reply(tree_cookie) {
                    Ok(tree) => {
                        let parent_id = xw.resource_id();

                        tree.children()
                            .iter()
                            .map(|&cxw| get_window_cookies(ctx, parent_id, cxw))
                            .into_iter()
                            .flatten()
                            .chain(std::iter::once(cookies))
                            .collect()
                    }
                    Err(e) => {
                        warn!("QueryTree for window {:?} failed: {}", xw, e);
                        vec![cookies]
                    }
                }
            }

            for wc in get_window_cookies(self, self.root.resource_id(), self.root) {
                let geom = self.conn.wait_for_reply(wc.geom);
                let state_prop = self.conn.wait_for_reply(wc.state_prop);
                let type_prop = self.conn.wait_for_reply(wc.type_prop);
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

                        wg.parents.insert(id, wc.parent_id);

                        // only take top-level client windows. ICCCM mandates that they will have a
                        // WM_STATE property, so any that don't are WM frames, housekeeping or other
                        // nonsense and not interesting for layout. WM_STATE==1 is NormalState; its rare to
                        // see anything else but might as well be defensive.
                        //
                        // we also take the root here.  it is doesn't have WM_STATE, but we still want its
                        // need its geometry

                        let w = Window {
                            id: id,
                            xw: wc.xw,
                            geom: Rect::new(
                                (geom.x(), geom.y()).into(),
                                (geom.width() as i16, geom.height() as i16).into(),
                            ),
                            typ: match wc.xw == self.root {
                                true => WindowType::Root,
                                false => match type_prop.length() {
                                    // some clients (Spotify) do not set a _NET_WM_WINDOW_TYPE at all.
                                    // we already. we just treat them as TYPE_NORMAL here, because
                                    // unless they've been selected somehow it won't even matter.
                                    0 => WindowType::Normal,
                                    _ => match type_prop.value::<x::Atom>()[0] {
                                        v if v == self.atoms.net_wm_window_type_dock => {
                                            WindowType::Dock
                                        }
                                        v if v == self.atoms.net_wm_window_type_desktop => {
                                            WindowType::Desktop
                                        }
                                        _ => WindowType::Normal,
                                    },
                                },
                            },
                        };

                        if wc.xw == self.root
                            || state_prop.r#type() == self.atoms.wm_state
                                && state_prop.value::<u32>()[0] == 1
                        {
                            match w.typ {
                                WindowType::Normal => {
                                    // XXX actually drill down to child like the select thing does
                                    wg.selectable.insert(id);
                                    ()
                                }
                                WindowType::Dock => {
                                    wg.dock.insert(id);
                                    ()
                                }
                                WindowType::Desktop => {
                                    wg.desktop.insert(id);
                                    ()
                                }
                                WindowType::Root => wg.root = Some(id),
                            }
                        }

                        wg.windows.insert(id, w);
                    }
                }
            }

            wg
        })
    }

    fn x_query_tree(&self, xw: x::Window) -> x::QueryTreeCookie {
        self.conn.send_request(&x::QueryTree { window: xw })
    }

    fn x_get_geometry(&self, xw: x::Window) -> x::GetGeometryCookie {
        self.conn.send_request(&x::GetGeometry {
            drawable: x::Drawable::Window(xw),
        })
    }

    fn x_get_property(&self, xw: x::Window, prop: x::Atom, ty: x::Atom) -> x::GetPropertyCookie {
        self.conn.send_request(&x::GetProperty {
            window: xw,
            delete: false,
            property: prop,
            r#type: ty,
            long_offset: 0,
            long_length: 512,
        })
    }

    fn _window_name(&self, w: &Window) -> xcb::Result<String> {
        let name_prop = self.conn.wait_for_reply(self.x_get_property(
            w.xw,
            self.atoms.net_wm_name,
            x::ATOM_ANY,
        ))?;
        Ok(String::from_utf8_lossy(name_prop.value()).to_string())
    }

    fn window_abs_xlate(&self, w: &Window, wg: &WindowGroup) -> Vector2D {
        let mut id = w.id;
        let mut geom = w.geom;
        while id != self.root.resource_id() {
            id = wg.parents[&id];
            geom = geom.translate(self.window(id).geom.origin.to_vector());
        }
        geom.min() - w.geom.min()
    }

    fn window_frame_extents(&self, w: &Window, prop: x::Atom) -> xcb::Result<SideOffsets2D> {
        let extents_prop =
            self.conn
                .wait_for_reply(self.x_get_property(w.xw, prop, x::ATOM_CARDINAL))?;

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
                debug!("window {} has no extents {:?}, assuming zero", w.id, prop);
                Ok(SideOffsets2D::zero())
            }
        }
    }
}

fn main() -> xcb::Result<()> {
    let args = RootArgs::parse();

    let target_arg = if let Some(id) = args.id {
        TargetArgs::Id(id)
    } else if args.active {
        TargetArgs::Active
    } else if args.select {
        TargetArgs::Select
    } else {
        TargetArgs::None
    };

    env_logger::Builder::new().parse_default_env().init();

    let ctx = Context::new()?;

    let wg = ctx.window_group();
    //debug!("{:#?}", wg);

    for &desktop_id in wg.desktop.iter() {
        let desktop = ctx.window(desktop_id);
        debug!("desktop geom: {:?}", desktop.geom);
    }

    /*
    // stage 1: discover all visible windows

    // all the on-screen windows
    // XXX same workspace: _NET_WM_DESKTOP(CARDINAL)
    let all_windows = get_visible_windows(&ctx);

    //println!("{:#?}", all_windows);

    // split into regular windows that we can operate on, and special windows that we should try
    // not to cover
    let (normal_windows, dock_windows, desktop_windows, root_window) = all_windows.iter().fold(
        (vec![], vec![], vec![], None),
        |(mut normal, mut dock, mut desktop, mut root), w| {
            match w.typ {
                WindowType::Normal => normal.push(w),
                WindowType::Dock => dock.push(w),
                WindowType::Desktop => desktop.push(w),
                WindowType::Root => root = Some(w),
            }
            (normal, dock, desktop, root)
        },
    );
    */

    // stage 2: figure out the window they asked for, and fetch/compute its
    // size, frame extents, etc - everything we need to figure out how to place it
    //
    // we have to do this first, because we need its position so we can decide which desktop to use
    // as a reference
    let target_id = 'target: {
        let mut id = match target_arg {
            TargetArgs::Id(id) => id,
            TargetArgs::Active => {
                let active_prop = ctx.conn.wait_for_reply(ctx.x_get_property(
                    ctx.root,
                    ctx.atoms.net_active_window,
                    x::ATOM_WINDOW,
                ))?;
                active_prop.value()[0]
            }
            TargetArgs::Select => select_window(&ctx)?,
            TargetArgs::None => unreachable!(),
        };

        if wg.selectable.contains(&id) {
            break 'target id;
        }

        let orig_id = id;

        while id > 0 && id != ctx.root.resource_id() {
            debug!("requested window {} not selectable, checking parent", id);
            id = wg.parents[&id];
            if wg.selectable.contains(&id) {
                debug!("parent window {} selectable, using it", id);
                break 'target id;
            }
        }

        if let Some(id) = wg
            .parents
            .iter()
            .filter_map(
                |(&cid, &pid)| match pid == orig_id && wg.selectable.contains(&cid) {
                    true => Some(cid),
                    false => None,
                },
            )
            .next()
        {
            debug!("child window {} selectable, using it", id);
            break 'target id;
        }

        warn!(
            "couldn't resolve window id {} to a top client window",
            orig_id
        );
        return Ok(());
    };

    debug!("target window id: {}", target_id);

    let current_geom = ctx.window(target_id).geom;
    debug!("target geom: {:?}", current_geom);

    let current_box = current_geom.to_box2d();
    debug!("target current box: {:?}", current_box);

    let frame_extents =
        ctx.window_frame_extents(ctx.window(target_id), ctx.atoms.net_frame_extents)?;
    debug!("target frame extents: {:?}", frame_extents);

    let unframed_box = current_box.outer_box(frame_extents);
    debug!("target unframed box: {:?}", unframed_box);

    let current_box = unframed_box;

    let avail_box = match wg
        .desktop
        .iter()
        .filter_map(|&id| {
            let desktop = ctx
                .window(id)
                .geom
                .translate(ctx.window_abs_xlate(ctx.window(id), &wg))
                .to_box2d();
            debug!("desktop {} box: {:?}", id, desktop);
            desktop.contains(current_box.min).then(|| {
                debug!("target {} is on desktop {}", target_id, id);
                desktop
            })
        })
        .next()
    {
        Some(desktop) => desktop,
        None => {
            warn!("couldn't determine desktop for window id {}", target_id);
            return Ok(());
        }
    };

    debug!("initial avail box: {:?}", avail_box);

    let avail_box = wg.dock.iter().fold(avail_box, |avail, &id| {
        debug!("dock {} geom: {:?}", id, ctx.window(id).geom);
        let dock = ctx
            .window(id)
            .geom
            .translate(ctx.window_abs_xlate(ctx.window(id), &wg))
            .to_box2d();
        debug!("dock {} box: {:?}", id, dock);
        match avail.intersection(&dock) {
            Some(overlap) if overlap == avail => {
                debug!("dock {} covers avail area, ignoring it", id);
                avail
            }

            // dock doesn't intersect the area, ignore it
            None => {
                debug!("dock {} is outside avail area, ignoring it", id);
                avail
            }

            Some(overlap) => {
                debug!(
                    "dock {} overlaps avail, reducing (overlap {:?})",
                    id, overlap
                );

                let mut regions = vec![];
                if avail.min.x < overlap.min.x {
                    // left of dock
                    regions.push(Box2D::new(avail.min, (overlap.min.x, avail.max.y).into()));
                }
                if avail.max.x > overlap.max.x {
                    // right of dock
                    regions.push(Box2D::new((overlap.max.x, avail.min.y).into(), avail.max));
                }
                if avail.min.y < overlap.min.y {
                    // above dock
                    regions.push(Box2D::new(avail.min, (avail.max.x, overlap.min.y).into()));
                }
                if avail.max.y > overlap.max.y {
                    // below dock
                    regions.push(Box2D::new((avail.min.x, overlap.max.y).into(), avail.max));
                }

                // XXX guaranteed to have one. in some future nonsense, we'd select the "best" by
                // some means. I don't do docks really though, so nothing for now
                debug!("new avail regions (taking last): {:?}", regions);
                regions.pop().unwrap()
            }
        }
    });

    debug!("final avail box: {:?}", avail_box);

    let new_box = compute_new_box(&current_box, &avail_box, args.horiz, args.vert);
    debug!("target new unframed box: {:?}", new_box);

    let framed_box = new_box.inner_box(frame_extents);
    debug!("target new framed box: {:?}", framed_box);

    let offset_box = framed_box.translate((-frame_extents.left, -frame_extents.top).into());
    debug!("target new offset box: {:?}", offset_box);

    let new_geom = offset_box.to_rect();
    debug!("target new geom: {:?}", new_geom);

    let ev = {
        let target = ctx.window(target_id);

        x::ClientMessageEvent::new(
            target.xw,
            ctx.atoms.net_moveresize_window,
            x::ClientMessageData::Data32([
                (MoveResizeWindowFlags::X
                    | MoveResizeWindowFlags::Y
                    | MoveResizeWindowFlags::WIDTH
                    | MoveResizeWindowFlags::HEIGHT
                    | MoveResizeWindowFlags::GRAVITY_NORTH_WEST)
                    .bits(),
                new_geom.origin.x as u32,
                new_geom.origin.y as u32,
                new_geom.size.width as u32,
                new_geom.size.height as u32,
            ]),
        )
    };

    ctx.conn.send_request(&x::SendEvent {
        propagate: false,
        destination: x::SendEventDest::Window(ctx.root),
        event_mask: x::EventMask::SUBSTRUCTURE_REDIRECT | x::EventMask::SUBSTRUCTURE_NOTIFY,
        event: &ev,
    });

    ctx.conn.flush()?;

    Ok(())
}

fn select_window(ctx: &Context) -> xcb::Result<u32> {
    let font = ctx.conn.generate_id();
    ctx.conn.send_request(&x::OpenFont {
        fid: font,
        name: b"cursor",
    });

    let cursor = ctx.conn.generate_id();
    ctx.conn.send_request(&x::CreateGlyphCursor {
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

    ctx.conn
        .wait_for_reply(ctx.conn.send_request(&x::GrabPointer {
            owner_events: false,
            grab_window: ctx.root,
            event_mask: x::EventMask::BUTTON_PRESS | x::EventMask::BUTTON_RELEASE,
            pointer_mode: x::GrabMode::Sync,
            keyboard_mode: x::GrabMode::Async,
            confine_to: ctx.root,
            cursor: cursor,
            time: x::CURRENT_TIME,
        }))?;

    let selected = loop {
        ctx.conn.send_request(&x::AllowEvents {
            mode: x::Allow::SyncPointer,
            time: x::CURRENT_TIME,
        });
        ctx.conn.flush()?;

        if let xcb::Event::X(x::Event::ButtonPress(ev)) = ctx.conn.wait_for_event()? {
            let w = ev.child();
            if !w.is_none() {
                break w;
            }
        }
    };

    ctx.conn.send_request(&x::UngrabPointer {
        time: x::CURRENT_TIME,
    });
    ctx.conn.flush()?;

    Ok(selected.resource_id())
}

fn compute_new_box(current: &Box2D, avail: &Box2D, hspec: HorizSpec, vspec: VertSpec) -> Box2D {
    let (x1, x2) = compute_new_horiz(current, avail, hspec);
    let (y1, y2) = compute_new_vert(current, avail, vspec);
    Box2D::new((x1, y1).into(), (x2, y2).into())
}

fn compute_new_horiz(current: &Box2D, avail: &Box2D, hspec: HorizSpec) -> (i16, i16) {
    match hspec {
        HorizSpec::Current => (current.min.x, current.max.x),

        HorizSpec::Left25 => (avail.min.x, avail.min.x + avail.width().div_euclid(4)),
        HorizSpec::Left50 => (avail.min.x, avail.min.x + avail.width().div_euclid(2)),
        HorizSpec::Left75 => (avail.min.x, avail.min.x + (avail.width() * 3).div_euclid(4)),

        HorizSpec::Right25 => (avail.max.x - avail.width().div_euclid(4), avail.max.x),
        HorizSpec::Right50 => (avail.max.x - avail.width().div_euclid(2), avail.max.x),
        HorizSpec::Right75 => (avail.max.x - (avail.width() * 3).div_euclid(4), avail.max.x),

        HorizSpec::Full => (avail.min.x, avail.max.x),

        HorizSpec::Left => {
            let (x1_25, x2_25) = compute_new_horiz(current, avail, HorizSpec::Left25);
            let (x1_50, x2_50) = compute_new_horiz(current, avail, HorizSpec::Left50);
            let (x1_75, x2_75) = compute_new_horiz(current, avail, HorizSpec::Left75);

            if (current.min.x, current.max.x) == (x1_50, x2_50) {
                (x1_25, x2_25)
            } else if (current.min.x, current.max.x) == (x1_25, x2_25) {
                (x1_75, x2_75)
            } else {
                (x1_50, x2_50)
            }
        }

        HorizSpec::Right => {
            let (x1_25, x2_25) = compute_new_horiz(current, avail, HorizSpec::Right25);
            let (x1_50, x2_50) = compute_new_horiz(current, avail, HorizSpec::Right50);
            let (x1_75, x2_75) = compute_new_horiz(current, avail, HorizSpec::Right75);

            if (current.min.x, current.max.x) == (x1_50, x2_50) {
                (x1_25, x2_25)
            } else if (current.min.x, current.max.x) == (x1_25, x2_25) {
                (x1_75, x2_75)
            } else {
                (x1_50, x2_50)
            }
        }
    }
}

fn compute_new_vert(current: &Box2D, avail: &Box2D, vspec: VertSpec) -> (i16, i16) {
    match vspec {
        VertSpec::Current => (current.min.y, current.max.y),

        VertSpec::Top => (avail.min.y, avail.min.y + avail.height().div_euclid(2)),
        VertSpec::Bottom => (avail.max.y - avail.height().div_euclid(2), avail.max.y),

        VertSpec::Full => (avail.min.y, avail.max.y),
    }
}
