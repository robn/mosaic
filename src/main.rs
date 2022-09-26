use clap::{ArgGroup, Parser, ValueEnum};
use log::{debug, warn};
use xcb::{x, Xid};

xcb::atoms_struct! {
    #[derive(Copy, Clone, Debug)]
    struct Atoms {
        wm_state => b"WM_STATE",

        net_wm_window_type => b"_NET_WM_WINDOW_TYPE",
        net_wm_window_type_normal => b"_NET_WM_WINDOW_TYPE_NORMAL",
        net_wm_window_type_dock => b"_NET_WM_WINDOW_TYPE_DOCK",

        net_active_window => b"_NET_ACTIVE_WINDOW",

        net_frame_extents => b"_NET_FRAME_EXTENTS",
        gtk_frame_extents => b"_GTK_FRAME_EXTENTS",

        net_moveresize_window => b"_NET_MOVERESIZE_WINDOW",
    }
}

// XXX use ArgGroup enums for target: https://github.com/clap-rs/clap/issues/2621
#[derive(Parser, Debug)]
#[clap(group(ArgGroup::new("target").required(true)))]
struct RootArgs {
    #[clap(long, group = "target", parse(try_from_str=clap_num::maybe_hex))]
    id: Option<u32>,

    #[clap(long, group = "target")]
    active: bool,

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
    Active,
}

#[derive(Debug)]
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

fn main() -> xcb::Result<()> {
    let args = RootArgs::parse();

    let target_arg = if let Some(id) = args.id {
        TargetArgs::Id(id)
    } else if args.active {
        TargetArgs::Active
    } else {
        TargetArgs::None
    };

    env_logger::Builder::new().parse_default_env().init();

    // connect to server
    let (conn, scr_num) = xcb::Connection::connect(None)?;

    let atoms = Atoms::intern_all(&conn)?;

    // get screen handle
    let screen = conn
        .get_setup()
        .roots()
        .nth(scr_num as usize)
        .unwrap()
        .to_owned();

    // all the on-screen windows
    // XXX same workspace: _NET_WM_DESKTOP(CARDINAL)
    let all_windows = get_visible_windows(&conn, &atoms, screen.root())?;

    // split into regular windows that we can operate on, and special windows that we should try
    // not to cover
    let (normal_windows, dock_windows) = all_windows
        .iter()
        .map(|&w| {
            let typeprop_cookie = conn.send_request(&x::GetProperty {
                window: w,
                delete: false,
                property: atoms.net_wm_window_type,
                r#type: x::ATOM_ANY,
                long_offset: 0,
                long_length: 512,
            });
            (w, typeprop_cookie)
        })
        .map(|(w, typeprop_cookie)| {
            let typ = match conn.wait_for_reply(typeprop_cookie) {
                Ok(typeprop) => match typeprop.length() {
                    // some clients (Spotify) do not set a _NET_WM_WINDOW_TYPE at all. we already
                    // know this window has WM_STATE NormalState because we filtered for those
                    // windows earlier, so just pass it through as a TYPE_NORMAL window
                    0 => atoms.net_wm_window_type_normal,
                    _ => typeprop.value()[0],
                },
                Err(e) => {
                    debug!("{:?} couldn't get window type: {}", w, e);
                    atoms.net_wm_window_type_normal
                }
            };
            (w, typ)
        })
        .fold((vec![], vec![]), |(mut normal, mut dock), (w, typ)| {
            if typ == atoms.net_wm_window_type_normal {
                normal.push(w);
            } else if typ == atoms.net_wm_window_type_dock {
                dock.push(w);
            }
            (normal, dock)
        });

    // figure out the usable bounds
    let usable_bounds = {
        // first, the root
        let root_geom = get_window_geometry(&conn, &screen.root())?;
        let root_bounds = Bounds {
            x: root_geom.x(),
            y: root_geom.y(),
            w: root_geom.width() as i16,
            h: root_geom.height() as i16,
        };
        debug!("root bounds: {:?}", root_bounds);

        // top bar since that's what I actually have
        dock_windows
            .iter()
            .fold(Ok::<Bounds, xcb::Error>(root_bounds), |bounds, w| {
                match bounds {
                    Ok(mut bounds) => {
                        let geom = get_window_geometry(&conn, w)?;

                        // XXX hardcoded for my single top bar
                        bounds.y = geom.height() as i16;
                        bounds.h -= geom.height() as i16;

                        /* XXX actually do magic box intersection shit
                        let b = Bounds {
                            x: geom.x(),
                            y: geom.y(),
                            w: geom.width(),
                            h: geom.height(),
                        };

                        debug!("dock bounds: {:?}", b);

                        ... what now?
                        */

                        Ok(bounds)
                    }
                    e => e,
                }
            })?
    };
    debug!("usable screen bounds: {:?}", usable_bounds);

    // figure out the window they asked for
    let id = match target_arg {
        TargetArgs::Id(id) => id,
        TargetArgs::Active => {
            let activeprop = conn.wait_for_reply(conn.send_request(&x::GetProperty {
                window: screen.root(),
                delete: false,
                property: atoms.net_active_window,
                r#type: x::ATOM_WINDOW,
                long_offset: 0,
                long_length: 512,
            }))?;
            activeprop.value()[0]
        }
        TargetArgs::None => unreachable!(),
    };
    debug!("requested window id: {}", id);

    // and match it to an actual window
    let w = match normal_windows
        .iter()
        .filter(|&w| w.resource_id() == id)
        .next()
    {
        Some(w) => w,
        _ => {
            warn!("requested window {} not found", id);
            return Ok(());
        }
    };

    // and get its bounds
    let window_bounds = {
        let geom = get_window_geometry(&conn, w)?;
        let xlate = conn.wait_for_reply(conn.send_request(&x::TranslateCoordinates {
            src_window: *w,
            dst_window: screen.root(),
            src_x: 0,
            src_y: 0,
        }))?;
        Bounds {
            x: xlate.dst_x(),
            y: xlate.dst_y(),
            w: geom.width() as i16,
            h: geom.height() as i16,
        }
    };
    debug!("window bounds: {:?}", window_bounds);

    let frame_extents = get_frame_extents(&conn, &atoms, w)?;
    debug!("frame extents: {:?}", frame_extents);

    let offset_window_bounds = Bounds {
        x: window_bounds.x - frame_extents.left,
        y: window_bounds.y - frame_extents.top,
        w: window_bounds.w + frame_extents.left + frame_extents.right,
        h: window_bounds.h + frame_extents.top + frame_extents.bottom,
    };
    debug!("offset window bounds: {:?}", offset_window_bounds);

    let target_bounds =
        compute_target_bounds(&offset_window_bounds, &usable_bounds, args.horiz, args.vert);
    debug!("target bounds: {:?}", target_bounds);

    let final_bounds = Bounds {
        x: target_bounds.x,
        y: target_bounds.y,
        w: target_bounds.w - frame_extents.left - frame_extents.right,
        h: target_bounds.h - frame_extents.top - frame_extents.bottom,
    };
    debug!("final bounds: {:?}", final_bounds);

    let ev = x::ClientMessageEvent::new(
        *w,
        atoms.net_moveresize_window,
        x::ClientMessageData::Data32([
            (MoveResizeWindowFlags::X
                | MoveResizeWindowFlags::Y
                | MoveResizeWindowFlags::WIDTH
                | MoveResizeWindowFlags::HEIGHT
                | MoveResizeWindowFlags::GRAVITY_NORTH_WEST)
                .bits(),
            final_bounds.x as u32,
            final_bounds.y as u32,
            final_bounds.w as u32,
            final_bounds.h as u32,
        ]),
    );

    conn.send_request(&x::SendEvent {
        propagate: false,
        destination: x::SendEventDest::Window(screen.root()),
        event_mask: x::EventMask::SUBSTRUCTURE_REDIRECT | x::EventMask::SUBSTRUCTURE_NOTIFY,
        event: &ev,
    });

    conn.flush()?;

    Ok(())
}

// find out about all the windows
fn get_visible_windows(
    conn: &xcb::Connection,
    atoms: &Atoms,
    w: x::Window,
) -> xcb::Result<Vec<x::Window>> {
    let tree = conn.wait_for_reply(conn.send_request(&x::QueryTree { window: w }))?;
    let mut windows: Vec<x::Window> = tree
        .children()
        .iter()
        .map(|&w| match get_visible_windows(conn, atoms, w) {
            Ok(v) => v,
            Err(e) => {
                warn!("QueryTree for window {:?} failed: {}", w, e);
                vec![]
            }
        })
        .collect::<Vec<Vec<x::Window>>>()
        .into_iter()
        .flatten()
        .collect();

    let stateprop = conn.wait_for_reply(conn.send_request(&x::GetProperty {
        window: w,
        delete: false,
        property: atoms.wm_state,
        r#type: atoms.wm_state,
        long_offset: 0,
        long_length: 512,
    }))?;
    if stateprop.r#type() == atoms.wm_state {
        let state: u32 = stateprop.value()[0];
        if state == 1 {
            // NormalState
            windows.push(w);
        }
    }

    Ok(windows)
}

fn get_window_geometry(conn: &xcb::Connection, w: &x::Window) -> xcb::Result<x::GetGeometryReply> {
    conn.wait_for_reply(conn.send_request(&x::GetGeometry {
        drawable: x::Drawable::Window(*w),
    }))
}

fn get_frame_extents(conn: &xcb::Connection, atoms: &Atoms, w: &x::Window) -> xcb::Result<Extents> {
    let net_extents = get_frame_extents_prop(conn, atoms.net_frame_extents, w)?;
    /*
    let gtk_extents = get_frame_extents_prop(conn, atoms.gtk_frame_extents, w)?;
    Ok(Extents {
        left: net_extents.left - gtk_extents.left,
        right: net_extents.right - gtk_extents.right,
        top: net_extents.top - gtk_extents.top,
        bottom: net_extents.bottom - gtk_extents.bottom,
    })
    */
    Ok(net_extents)
}

fn get_frame_extents_prop(
    conn: &xcb::Connection,
    prop: x::Atom,
    w: &x::Window,
) -> xcb::Result<Extents> {
    let extentsprop = conn.wait_for_reply(conn.send_request(&x::GetProperty {
        window: *w,
        delete: false,
        property: prop,
        r#type: x::ATOM_CARDINAL,
        long_offset: 0,
        long_length: 512,
    }))?;

    let extents = match extentsprop.r#type() {
        x::ATOM_CARDINAL => {
            let v: &[u32] = extentsprop.value();
            Extents {
                left: v[0] as i16,
                right: v[1] as i16,
                top: v[2] as i16,
                bottom: v[3] as i16,
            }
        }
        _ => {
            debug!("{:?} has no extents {:?}, assuming zero", w, prop);
            Extents {
                left: 0,
                right: 0,
                top: 0,
                bottom: 0,
            }
        }
    };

    debug!("{:?} extents {:?}: {:?}", w, prop, extents);

    Ok(extents)
}

fn compute_target_bounds(
    current: &Bounds,
    usable: &Bounds,
    horiz: HorizSpec,
    vert: VertSpec,
) -> Bounds {
    let (x, w) = compute_target_horiz_bounds(current, usable, horiz);
    let (y, h) = compute_target_vert_bounds(current, usable, vert);
    Bounds { x, y, w, h }
}

fn compute_target_horiz_bounds(current: &Bounds, usable: &Bounds, horiz: HorizSpec) -> (i16, i16) {
    match horiz {
        HorizSpec::Current => (current.x, current.w),

        HorizSpec::Left25 => (usable.x, usable.w.div_euclid(4)),
        HorizSpec::Left50 => (usable.x, usable.w.div_euclid(2)),
        HorizSpec::Left75 => (usable.x, (usable.w * 3).div_euclid(4)),

        HorizSpec::Right25 => (
            usable.x + ((usable.w as i16) * 3).div_euclid(4),
            usable.w.div_euclid(4),
        ),
        HorizSpec::Right50 => (
            usable.x + (usable.w as i16).div_euclid(2),
            usable.w.div_euclid(2),
        ),
        HorizSpec::Right75 => (
            usable.x + (usable.w as i16).div_euclid(4),
            (usable.w * 3).div_euclid(4),
        ),

        HorizSpec::Full => (usable.x, usable.w),

        HorizSpec::Left => {
            let (x25, w25) = compute_target_horiz_bounds(current, usable, HorizSpec::Left25);
            let (x50, w50) = compute_target_horiz_bounds(current, usable, HorizSpec::Left50);
            let (x75, w75) = compute_target_horiz_bounds(current, usable, HorizSpec::Left75);

            if (current.x, current.w) == (x50, w50) {
                (x25, w25)
            } else if (current.x, current.w) == (x25, w25) {
                (x75, w75)
            } else {
                (x50, w50)
            }
        }

        HorizSpec::Right => {
            let (x25, w25) = compute_target_horiz_bounds(current, usable, HorizSpec::Right25);
            let (x50, w50) = compute_target_horiz_bounds(current, usable, HorizSpec::Right50);
            let (x75, w75) = compute_target_horiz_bounds(current, usable, HorizSpec::Right75);

            if (current.x, current.w) == (x50, w50) {
                (x25, w25)
            } else if (current.x, current.w) == (x25, w25) {
                (x75, w75)
            } else {
                (x50, w50)
            }
        }
    }
}

fn compute_target_vert_bounds(current: &Bounds, usable: &Bounds, vert: VertSpec) -> (i16, i16) {
    match vert {
        VertSpec::Current => (current.y, current.h),
        VertSpec::Top => (usable.y, usable.h.div_euclid(2)),
        VertSpec::Bottom => (
            usable.y + (usable.h as i16).div_euclid(2),
            usable.h.div_euclid(2),
        ),
        VertSpec::Full => (usable.y, usable.h),
    }
}
