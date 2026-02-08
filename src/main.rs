mod geom;
mod session;

use crate::geom::*;
use crate::session::Session;

use anyhow::{Context, Result};
use clap::{ArgGroup, Parser, ValueEnum};
use log::debug;

fn percent(s: &str) -> Result<i16, String> {
    clap_num::number_range(s, 0, 100)
}

// XXX use ArgGroup enums: https://github.com/clap-rs/clap/issues/2621
#[derive(Parser, Debug)]
#[clap(group(ArgGroup::new("target").required(true)))]
struct RootArgs {
    #[clap(long, group = "target", value_parser=clap_num::maybe_hex::<u32>)]
    id: Option<u32>,

    #[clap(long, group = "target")]
    active: bool,

    #[clap(long, group = "target")]
    select: bool,

    #[clap(long)]
    halign: Option<HorizAlignArgs>,

    #[clap(long)]
    valign: Option<VertAlignArgs>,

    #[clap(long, value_parser=percent, default_value=None)]
    width: Option<i16>,
    #[clap(long, value_parser=percent, default_value=None)]
    height: Option<i16>,
}

#[derive(Debug)]
enum TargetArgs {
    None,
    Id(u32),
    Select,
    Active,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum HorizAlignArgs {
    Left,
    Middle,
    Right,
}
#[derive(ValueEnum, Clone, Copy, Debug)]
enum VertAlignArgs {
    Top,
    Middle,
    Bottom,
}

fn main() -> Result<()> {
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

    let sess = Session::init().context("failed to connect to X11 server")?;

    let target_id = 'target: {
        let w = match target_arg {
            TargetArgs::Id(id) => sess.window(id),
            TargetArgs::Active => sess
                .active_window()
                .context("failed to get active window")?,
            TargetArgs::Select => sess.select_window().context("failed to select window")?,
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

        anyhow::bail!(
            "couldn't resolve target {:?} to a selectable window",
            target_arg
        );
    };

    debug!("target window id: {}", target_id);

    let target = sess.window(target_id);

    let frame = target
        .frame_extents()
        .context("failed to get window frame extents")?;
    debug!("target frame extents: {:?}", frame);

    let current_geom = {
        let geom = target.abs_geom();
        debug!("target geom: {:?}", geom);

        let unframed = geom.outer_box(frame);
        debug!("target unframed geom: {:?}", unframed);

        unframed
    };

    let avail_geom = sess
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
        .with_context(|| {
            format!(
                "couldn't determine which desktop contains window {}",
                target_id
            )
        })?;

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
        let geom = compute_new_geom(&current_geom, &avail_geom, &args);
        debug!(
            "computed new geom (halign={:?} width={:?} valign={:?} height={:?}): {:?}",
            args.halign, args.valign, args.width, args.height, geom
        );

        let framed = geom.inner_box(frame);
        debug!("computed new framed geom: {:?}", framed);

        framed
    };

    sess.window(target_id)
        .set_geom(&new_geom)
        .context("failed to move/resize window")?;

    Ok(())
}

fn compute_new_geom(current: &Box2D, avail: &Box2D, args: &RootArgs) -> Box2D {
    let w = match args.width {
        Some(p) if p == 0 => 1,
        Some(p) if p == 100 => avail.width(),
        Some(p) => (avail.width() as i32 * p as i32).div_euclid(100) as i16,
        None => current.width(),
    };

    let h = match args.height {
        Some(p) if p == 0 => 1,
        Some(p) if p == 100 => avail.height(),
        Some(p) => (avail.height() as i32 * p as i32).div_euclid(100) as i16,
        None => current.height(),
    };

    let x = match args.halign {
        Some(p) => match p {
            HorizAlignArgs::Left => avail.min.x,
            HorizAlignArgs::Middle => (avail.width() - w).div_euclid(2),
            HorizAlignArgs::Right => avail.max.x - w,
        },
        None => current.min.x,
    };

    let y = match args.valign {
        Some(p) => match p {
            VertAlignArgs::Top => avail.min.y,
            VertAlignArgs::Middle => (avail.height() - h).div_euclid(2),
            VertAlignArgs::Bottom => avail.max.y - h,
        },
        None => current.min.y,
    };

    Box2D::from_origin_and_size((x, y).into(), (w, h).into())
}
