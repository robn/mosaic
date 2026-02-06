mod geom;
mod session;

use crate::geom::*;
use crate::session::Session;

use clap::{ArgGroup, Parser, ValueEnum};
use log::{debug, warn};
use xcb::{x, Xid};

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
    } else if args.select {
        TargetArgs::Select
    } else {
        TargetArgs::None
    };

    env_logger::Builder::new().parse_default_env().init();

    let sess = Session::init()?;

    for &desktop_id in sess.desktops() {
        let desktop = sess.window(desktop_id);
        debug!("desktop geom: {:?}", desktop.geom);
    }

    /*
    // stage 1: discover all visible windows

    // all the on-screen windows
    // XXX same workspace: _NET_WM_DESKTOP(CARDINAL)
    let all_windows = get_visible_windows(&sess);

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
        let id = match target_arg {
            TargetArgs::Id(id) => id,
            TargetArgs::Active => {
                let active_prop = sess.conn().wait_for_reply(sess.x_get_property(
                    sess.root(),
                    sess.atoms().net_active_window,
                    x::ATOM_WINDOW,
                ))?;
                active_prop.value()[0]
            }
            TargetArgs::Select => select_window(&sess)?,
            TargetArgs::None => unreachable!(),
        };

        let w = sess.window(id);
        if w.selectable {
            break 'target id;
        }

        let mut parent = w.parent;
        while parent > 0 && parent != sess.root().resource_id() {
            debug!(
                "requested window {} not selectable, checking parent",
                parent
            );
            let pw = sess.window(parent);
            if pw.selectable {
                debug!("parent window {} selectable, using it", parent);
                break 'target parent;
            }
            parent = pw.parent;
        }

        if let Some(child) = w
            .children
            .iter()
            .filter_map(|&cid| sess.window(cid).selectable.then_some(cid))
            .next()
        {
            debug!("child window {} selectable, using it", child);
            break 'target child;
        }

        warn!("couldn't resolve window id {} to a top client window", id);
        return Ok(());
    };

    debug!("target window id: {}", target_id);

    let current_geom = sess.window(target_id).geom;
    debug!("target geom: {:?}", current_geom);

    let current_box = current_geom.to_box2d();
    debug!("target current box: {:?}", current_box);

    let frame_extents = sess.window(target_id).frame_extents()?;
    debug!("target frame extents: {:?}", frame_extents);

    let unframed_box = current_box.outer_box(frame_extents);
    debug!("target unframed box: {:?}", unframed_box);

    let current_box = unframed_box;

    let avail_box = match sess
        .desktops()
        .filter_map(|&id| {
            let w = sess.window(id);
            let desktop = w.geom.translate(w.abs_xlate()).to_box2d();
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

    let avail_box = sess.docks().fold(avail_box, |avail, &id| {
        let w = sess.window(id);
        let dock = w.geom.translate(w.abs_xlate()).to_box2d();
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
        let target = sess.window(target_id);

        x::ClientMessageEvent::new(
            target.xw,
            sess.atoms().net_moveresize_window,
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

    sess.conn().send_request(&x::SendEvent {
        propagate: false,
        destination: x::SendEventDest::Window(sess.root()),
        event_mask: x::EventMask::SUBSTRUCTURE_REDIRECT | x::EventMask::SUBSTRUCTURE_NOTIFY,
        event: &ev,
    });

    sess.conn().flush()?;

    Ok(())
}

fn select_window(sess: &Session) -> xcb::Result<u32> {
    let font = sess.conn().generate_id();
    sess.conn().send_request(&x::OpenFont {
        fid: font,
        name: b"cursor",
    });

    let cursor = sess.conn().generate_id();
    sess.conn().send_request(&x::CreateGlyphCursor {
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

    sess.conn()
        .wait_for_reply(sess.conn().send_request(&x::GrabPointer {
            owner_events: false,
            grab_window: sess.root(),
            event_mask: x::EventMask::BUTTON_PRESS | x::EventMask::BUTTON_RELEASE,
            pointer_mode: x::GrabMode::Sync,
            keyboard_mode: x::GrabMode::Async,
            confine_to: sess.root(),
            cursor: cursor,
            time: x::CURRENT_TIME,
        }))?;

    let selected = loop {
        sess.conn().send_request(&x::AllowEvents {
            mode: x::Allow::SyncPointer,
            time: x::CURRENT_TIME,
        });
        sess.conn().flush()?;

        if let xcb::Event::X(x::Event::ButtonPress(ev)) = sess.conn().wait_for_event()? {
            let w = ev.child();
            if !w.is_none() {
                break w;
            }
        }
    };

    sess.conn().send_request(&x::UngrabPointer {
        time: x::CURRENT_TIME,
    });
    sess.conn().flush()?;

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
