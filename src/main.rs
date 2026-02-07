mod geom;
mod session;

use crate::geom::*;
use crate::session::Session;

use clap::{ArgGroup, Parser, ValueEnum};
use log::{debug, warn};

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
    Left33,
    Mid33,
    Right33,
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

    let target_id = 'target: {
        let w = match target_arg {
            TargetArgs::Id(id) => sess.window(id),
            TargetArgs::Active => sess.active_window()?,
            TargetArgs::Select => sess.select_window()?,
            TargetArgs::None => unreachable!(),
        };

        if w.selectable {
            break 'target w.id;
        }

        let mut parent = w.parent;
        while parent > 0 && parent != sess.root().id {
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

        warn!(
            "couldn't resolve target {:?} to a top client window",
            target_arg
        );
        return Ok(());
    };

    debug!("target window id: {}", target_id);

    let target = sess.window(target_id);

    let frame = target.frame_extents()?;
    debug!("target frame extents: {:?}", frame);

    let current_geom = {
        let geom = target.abs_geom();
        debug!("target geom: {:?}", geom);

        let unframed = geom.outer_box(frame);
        debug!("target unframed geom: {:?}", unframed);

        unframed
    };

    let avail_geom = match sess
        .desktops()
        .filter_map(|&id| {
            let w = sess.window(id);
            let geom = w.abs_geom();
            debug!("desktop {} box: {:?}", id, geom);
            geom.intersects(&current_geom).then(|| {
                debug!("target {} is on desktop {}", target_id, id);
                geom
            })
        })
        // XXX take the first one. better probably would be to overlap with the desktop, and take
        // the one that has the largest overlap. or some other notion of "best" idk
        .next()
    {
        Some(geom) => geom,
        None => {
            warn!("couldn't determine desktop for window id {}", target_id);
            return Ok(());
        }
    };

    debug!("desktop avail geom: {:?}", avail_geom);

    let avail_geom = sess.docks().fold(avail_geom, |avail, &id| {
        let w = sess.window(id);
        let geom = w.abs_geom();
        debug!("dock {} box: {:?}", id, geom);
        match avail.intersection(&geom) {
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

    debug!("avail geom: {:?}", avail_geom);

    let new_geom = {
        let geom = compute_new_geom(&current_geom, &avail_geom, args.horiz, args.vert);
        debug!(
            "computed new geom (hspec={:?} vspec={:?}: {:?}",
            args.horiz, args.vert, geom
        );

        let framed = geom.inner_box(frame);
        debug!("computed new framed geom: {:?}", framed);

        framed
    };

    sess.window(target_id).set_geom(&new_geom)?;

    Ok(())
}

fn compute_new_geom(current: &Box2D, avail: &Box2D, hspec: HorizSpec, vspec: VertSpec) -> Box2D {
    let (x1, x2) = compute_new_horiz(current, avail, hspec);
    let (y1, y2) = compute_new_vert(current, avail, vspec);
    Box2D::new((x1, y1).into(), (x2, y2).into())
}

fn compute_new_horiz(current: &Box2D, avail: &Box2D, hspec: HorizSpec) -> (i16, i16) {
    match hspec {
        // Don't modify horizontal
        HorizSpec::Current => (current.min.x, current.max.x),

        // Leftmost 25/50/75%
        HorizSpec::Left25 => (avail.min.x, avail.min.x + avail.width().div_euclid(4)),
        HorizSpec::Left50 | HorizSpec::Left => {
            (avail.min.x, avail.min.x + avail.width().div_euclid(2))
        }
        HorizSpec::Left75 => (avail.min.x, avail.min.x + (avail.width() * 3).div_euclid(4)),

        // Rightmost 25/50/75%
        HorizSpec::Right25 => (avail.max.x - avail.width().div_euclid(4), avail.max.x),
        HorizSpec::Right50 | HorizSpec::Right => {
            (avail.max.x - avail.width().div_euclid(2), avail.max.x)
        }
        HorizSpec::Right75 => (avail.max.x - (avail.width() * 3).div_euclid(4), avail.max.x),

        // Thirds
        HorizSpec::Left33 => (avail.min.x, avail.max.x - (avail.width() * 2).div_euclid(3)),
        HorizSpec::Mid33 => {
            let w = avail.width().div_euclid(3);
            (avail.min.x + w, avail.max.x - w)
        }
        HorizSpec::Right33 => (avail.min.x + (avail.width() * 2).div_euclid(3), avail.max.x),

        // Full width
        HorizSpec::Full => (avail.min.x, avail.max.x),
    }
}

fn compute_new_vert(current: &Box2D, avail: &Box2D, vspec: VertSpec) -> (i16, i16) {
    match vspec {
        // Don't modify vertical
        VertSpec::Current => (current.min.y, current.max.y),

        // Top & bottom half
        VertSpec::Top => (avail.min.y, avail.min.y + avail.height().div_euclid(2)),
        VertSpec::Bottom => (avail.max.y - avail.height().div_euclid(2), avail.max.y),

        // Full height
        VertSpec::Full => (avail.min.y, avail.max.y),
    }
}
