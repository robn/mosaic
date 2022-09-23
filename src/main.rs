use clap::{Parser, ValueEnum};
use log::{debug, warn};
use xcb::{x, Xid};

xcb::atoms_struct! {
    #[derive(Copy, Clone, Debug)]
    pub(crate) struct Atoms {
        pub utf8_string => b"UTF8_STRING",

        pub wm_state => b"WM_STATE",

        pub net_wm_name => b"_NET_WM_NAME",

        pub net_wm_window_type => b"_NET_WM_WINDOW_TYPE",
        pub net_wm_window_type_normal => b"_NET_WM_WINDOW_TYPE_NORMAL",
        pub net_wm_window_type_dock => b"_NET_WM_WINDOW_TYPE_DOCK",
    }
}

#[derive(Parser, Debug)]
struct Args {
    #[clap(long, parse(try_from_str=clap_num::maybe_hex))]
    id: u32,

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
struct Bounds {
    x: i16,
    y: i16,
    w: u16,
    h: u16,
}

fn main() -> xcb::Result<()> {
    let args = Args::parse();

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
    //debug!("{:#?}", all_windows);

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
                Ok(typeprop) => typeprop.value()[0],
                Err(_) => x::ATOM_NONE,
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
            w: root_geom.width(),
            h: root_geom.height(),
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
                        bounds.h -= geom.height();

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

    // find the wanted window
    let w = match normal_windows
        .iter()
        .filter(|&w| w.resource_id() == args.id)
        .next()
    {
        Some(w) => w,
        _ => {
            warn!("requested window {} not found", args.id);
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
            w: geom.width(),
            h: geom.height(),
        }
    };
    debug!("window bounds: {:?}", window_bounds);

    let target_bounds =
        compute_target_bounds(&window_bounds, &usable_bounds, args.horiz, args.vert);
    debug!("target bounds: {:?}", target_bounds);

    conn.send_request(&x::ConfigureWindow {
        window: *w,
        value_list: &[
            x::ConfigWindow::X(target_bounds.x.into()),
            x::ConfigWindow::Y(target_bounds.y.into()),
            x::ConfigWindow::Width(target_bounds.w.into()),
            x::ConfigWindow::Height(target_bounds.h.into()),
        ],
    });
    conn.flush()?;

    /*
    dock_windows.iter().for_each(|&w| {
        let nameprop_cookie = conn.send_request(&x::GetProperty {
            window: w,
            delete: false,
            property: atoms.net_wm_name,
            r#type: atoms.utf8_string,
            long_offset: 0,
            long_length: 512,
        });
        let typeprop_cookie = conn.send_request(&x::GetProperty {
            window: w,
            delete: false,
            property: atoms.net_wm_window_type,
            r#type: x::ATOM_ANY,
            long_offset: 0,
            long_length: 512,
        });

        let name = match conn.wait_for_reply(nameprop_cookie) {
            Ok(nameprop) => String::from_utf8(nameprop.value().to_vec()).expect("[invalid]"),
            Err(_) => "[error]".to_string(),
        };
        let r#type = match conn.wait_for_reply(typeprop_cookie) {
            Ok(typeprop) => typeprop.value()[0],
            Err(_) => x::ATOM_NONE,
        };
        debug!("{:?} name {:?} type {:?}", w, name, r#type);
    });
    */

    /*
    all_windows.iter().for_each(|(id, &w)| {
        let nameprop_cookie = conn.send_request(&x::GetProperty {
            window: w,
            delete: false,
            property: atoms.net_wm_name,
            r#type: atoms.utf8_string,
            long_offset: 0,
            long_length: 512,
        });
        if let Ok(nameprop) = conn.wait_for_reply(nameprop_cookie) {
            let name = std::str::from_utf8(nameprop.value()).expect("[invalid]");
            debug!("window {:?}, name {:?}", w, name);
        }
    });
    */
    /*
    let attrs = get_window_attributes(&conn, w)?;
    debug!("window {:?}, attrs {:#?}", w, attrs);

    let geom = get_window_geometry(&conn, w)?;
    debug!("window {:?}, geom {:#?}", w, geom);

    let xlate_cookie = conn.send_request(&x::TranslateCoordinates {
        src_window: *w,
        dst_window: screen.root(),
        src_x: 0,
        src_y: 0,
    });
    let xlate = conn.wait_for_reply(xlate_cookie)?;
    debug!("window {:?}, xlate {:#?}", w, xlate);
    */

    /*
    conn.send_request(&x::ConfigureWindow {
        window: *w,
        value_list: &[
            x::ConfigWindow::X(5),
            x::ConfigWindow::Y(56),
        ],
    });
    conn.flush()?;
    */

    /*
    let prop_cookie = conn.send_request(&x::ListProperties { window: *w });
    if let Ok(props) = conn.wait_for_reply(prop_cookie) {
        debug!("window {:?}, props {:#?}", w, props);
    }
    */

    /*
    let typeprop = conn.wait_for_reply(conn.send_request(&x::GetProperty {
        window: *w,
        delete: false,
        property: atoms.net_wm_window_type,
        r#type: x::ATOM_ANY,
        long_offset: 0,
        long_length: 512,
    }))?;
    debug!("{:#?}", typeprop);
    */

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

fn get_window_attributes(
    conn: &xcb::Connection,
    w: &x::Window,
) -> xcb::Result<x::GetWindowAttributesReply> {
    conn.wait_for_reply(conn.send_request(&x::GetWindowAttributes { window: *w }))
}

fn get_window_geometry(conn: &xcb::Connection, w: &x::Window) -> xcb::Result<x::GetGeometryReply> {
    conn.wait_for_reply(conn.send_request(&x::GetGeometry {
        drawable: x::Drawable::Window(*w),
    }))
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

fn compute_target_horiz_bounds(current: &Bounds, usable: &Bounds, horiz: HorizSpec) -> (i16, u16) {
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

fn compute_target_vert_bounds(current: &Bounds, usable: &Bounds, vert: VertSpec) -> (i16, u16) {
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
