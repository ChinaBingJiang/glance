//! The dock's shape and its painting: where its tiles are along the row, and how they are drawn. No window, no
//! pointer, nothing running — those are the dock's own (see `crate::dock`).
//!
//! A tile the pointer grows near swells where it stands and leans over its neighbours, rather than pushing the whole
//! row along with it. The row is laid out once, at rest, and never moves: what the pointer does is to change how big
//! each tile is drawn in the place it was given. So the tile under the pointer is the tile under it at the end of the
//! swell too, and a press lands where the eye says it will — which a row that shifted under the hand never does.
//!
//! What is drawn and what is taken for a press are therefore the same tiles, in the same order: the least swollen
//! drawn first, the most last, over the rest. A press belongs to the last of those that cover it.
//!
//! The glass is measured as the row lies, and never changes while the dock is up: what the pointer swells rises out of
//! its top, past its edge, as the tile leans over its neighbours. The window is shaped to take the pointer over that
//! overflow as well, so a tile is pressed where it is seen even where it has left the glass behind.

use std::cmp::Ordering;

use crate::settings::DockDoor;
use crate::ui::canvas::{Canvas, Color, Fill, Picture, Point};
use crate::ui::icons::Icon;
use crate::ui::overlay::{Glass, Tone};
use crate::ui::text::Lang;

/// How far the glass keeps from its screen's edge (DIPs).
pub const INSET: f32 = 10.0;
/// The room round the glass its shadow falls in (DIPs).
pub const MARGIN: f32 = 14.0;
/// The glass's corners (DIPs), and the least room the tiles have inside it at its ends and along its thickness.
pub const RADIUS: f32 = 18.0;
pub const PAD: f32 = 8.0;
/// How much of a tile's side the mark it is drawn with fills, where it has no picture of its own.
pub const MARK: f32 = 0.5;
/// The dot under the tile of what is running: its radius, and how far in from the glass's edge (DIPs).
const DOT: f32 = 2.0;
const DOT_IN: f32 = 4.0;
/// How much a tile's corners are rounded, as its side (a folder's picture, an icon's own corners).
const TILE_RADIUS: f32 = 0.14;

/// The colours of a tone.
struct Palette {
    /// The tint.
    tint: u32,
    /// What is drawn on the glass: a door's mark, the Bin's.
    ink: Color,
    /// A gap in the row.
    faint: Color,
    /// A plate under a tile: a door's own, and the one the hand is on.
    tile: Color,
    /// The dot of what is running.
    dot: Color,
    /// The glass's rim: bright where light catches it at the top, fainter down its sides.
    rim: (Color, Color),
    /// Its shadows: a wide soft one and a tight one under its edge (colour, blur, drop).
    shadows: [(Color, f32, f32); 2],
}

const DARK: Palette = Palette {
    tint: 0x1B1F26,
    ink: Color::hex(0xF2F5FA, 0.92),
    faint: Color::hex(0xFFFFFF, 0.5),
    tile: Color::hex(0xFFFFFF, 0.10),
    dot: Color::hex(0xFFFFFF, 0.88),
    rim: (Color::hex(0xFFFFFF, 0.32), Color::hex(0xFFFFFF, 0.10)),
    shadows: [
        (Color::hex(0x000000, 0.34), 8.0, 4.0),
        (Color::hex(0x000000, 0.24), 1.0, 1.0),
    ],
};

const LIGHT: Palette = Palette {
    tint: 0xF3F6FA,
    ink: Color::hex(0x1A1E24, 0.9),
    faint: Color::hex(0x000000, 0.46),
    tile: Color::hex(0x000000, 0.07),
    dot: Color::hex(0x1A1E24, 0.8),
    rim: (Color::hex(0xFFFFFF, 0.6), Color::hex(0x000000, 0.12)),
    shadows: [
        (Color::hex(0x000000, 0.24), 8.0, 4.0),
        (Color::hex(0x000000, 0.16), 1.0, 1.0),
    ],
};

/// The colours a tone is drawn in.
fn colours(tone: Tone) -> &'static Palette {
    match tone {
        Tone::Dark => &DARK,
        Tone::Light => &LIGHT,
    }
}

/// The glass while it is dragged: deep blue, whatever is behind, so it is read as in hand.
const DRAGGED: Glass = Glass {
    tone: Tone::Dark,
    opacity: 0.85,
    presence: 1.0,
};
const DRAGGED_TINT: u32 = 0x1D4F91;

/// One tile: where its middle stands along the row (DIPs, from the glass's left), and how far it has swelled.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Tile {
    pub centre: f32,
    pub scale: f32,
}

/// The row as it is drawn: every tile, and how large the glass has to be to hold them (DIPs).
#[derive(Clone, PartialEq, Debug)]
pub struct Row {
    pub tiles: Vec<Tile>,
    pub length: f32,
    pub thickness: f32,
}

/// What a press on a tile does. Kept apart from what the settings list a tile to be (see `DockItem`), because a
/// program running has a tile without being kept in them, and because what a press asks for is not what the row is
/// made of: a gap holds room and does nothing.
#[derive(Clone, PartialEq, Debug)]
pub enum Act {
    /// A thing kept at hand, opened by the shell.
    Launch(String),
    /// One of Glance's own doors.
    Door(DockDoor),
    /// The Recycle Bin.
    Trash,
    /// A program running: to its window.
    GoTo(String),
    /// Room in the row, nothing under the pointer.
    Gap,
}

/// One tile of the dock: what it is, what it is called, and how it is pictured.
#[derive(Clone, PartialEq, Debug)]
pub struct Entry {
    pub act: Act,
    /// Its name, said over it while the pointer is on it.
    pub name: String,
    /// Its picture, the shell's own; none where it is drawn with a mark.
    pub picture: Option<Picture>,
    /// Whether it is running now.
    pub running: bool,
}

impl Entry {
    /// A gap in the row: no side to draw, nothing to swell, nothing to press.
    pub fn gap(&self) -> bool {
        matches!(self.act, Act::Gap)
    }

    /// The mark it is drawn with where it has no picture.
    pub fn mark(&self) -> Option<Icon> {
        match &self.act {
            Act::Launch(target) => Some(kept_mark(target)),
            Act::Door(door) => Some(door_mark(*door)),
            Act::Trash => Some(Icon::Trash),
            Act::GoTo(path) => Some(kept_mark(path)),
            Act::Gap => None,
        }
    }

    /// The colour a door is drawn on: a macOS-style plate, a hue of its own for each. None for the Bin and what is
    /// kept, which stand on the faint plate as before.
    pub fn badge(&self) -> Option<Color> {
        match &self.act {
            Act::Door(door) => Some(Color::hex(door_colour(*door), 1.0)),
            Act::Launch(_) | Act::Trash | Act::GoTo(_) | Act::Gap => None,
        }
    }

    /// What it stands for: the thing kept at hand, or the window what is running belongs to. None for one of
    /// Glance's doors, the Bin, and room in the row.
    #[cfg(test)]
    pub fn target(&self) -> Option<&str> {
        match &self.act {
            Act::Launch(target) | Act::GoTo(target) => Some(target),
            Act::Door(_) | Act::Trash | Act::Gap => None,
        }
    }
}

/// How a thing kept at hand is marked where the shell has no picture for it: by what it is.
fn kept_mark(target: &str) -> Icon {
    if target.starts_with("https://") || target.starts_with("http://") {
        Icon::Link
    } else if std::path::Path::new(target).is_dir() {
        Icon::Folder
    } else {
        Icon::File
    }
}

/// How one of Glance's doors is marked.
fn door_mark(door: DockDoor) -> Icon {
    match door {
        // (The grid of every program; the workbench, which was drawn with it, takes the mark its own page in the
        // settings carries.)
        DockDoor::Launchpad => Icon::LayoutGrid,
        DockDoor::Monitor => Icon::Activity,
        DockDoor::Workbench => Icon::PencilRuler,
        DockDoor::Music => Icon::Music,
        DockDoor::Overlay => Icon::Gauge,
        DockDoor::Settings => Icon::Settings,
    }
}

/// The hue a door's plate is filled with — a colour of its own for each, as macOS gives every app one.
fn door_colour(door: DockDoor) -> u32 {
    match door {
        DockDoor::Launchpad => 0x6E56CF, // violet: the grid of every program
        DockDoor::Monitor => 0x1FA37A,   // green: the readings, a pulse
        DockDoor::Workbench => 0xE0862C, // amber: the tools
        DockDoor::Music => 0xE23A5E,     // rose: the music
        DockDoor::Overlay => 0x2E8BE6,   // blue: the overlay
        DockDoor::Settings => 0x8A8F98,  // grey: the gears
    }
}

/// A door's name, as the dock says it.
pub fn door_says(door: DockDoor, lang: Lang) -> &'static str {
    match door {
        DockDoor::Launchpad => {
            if lang == Lang::Zh {
                "启动台"
            } else {
                "Launchpad"
            }
        }
        DockDoor::Monitor => {
            if lang == Lang::Zh {
                "监控面板"
            } else {
                "Monitors"
            }
        }
        DockDoor::Workbench => {
            if lang == Lang::Zh {
                "工作台"
            } else {
                "Workbench"
            }
        }
        DockDoor::Music => {
            if lang == Lang::Zh {
                "音乐"
            } else {
                "Music"
            }
        }
        DockDoor::Overlay => {
            if lang == Lang::Zh {
                "游戏悬浮窗"
            } else {
                "Overlay"
            }
        }
        DockDoor::Settings => {
            if lang == Lang::Zh {
                "设置"
            } else {
                "Settings"
            }
        }
    }
}

/// What the Recycle Bin is called.
pub fn trash_says(lang: Lang) -> &'static str {
    if lang == Lang::Zh {
        "回收站"
    } else {
        "Recycle Bin"
    }
}

/// From one tile's middle to the next's, at rest (DIPs): how far the pointer's swell reaches.
pub fn pitch(size: f32, spacing: f32) -> f32 {
    size + spacing
}

/// How far past the glass a tile at full peak reaches: along the row, at either end of it, and across the glass away
/// from its edge. The glass is no bigger for it — what swells rises out of the glass rather than lifting it, which
/// would resize the window as the pointer came and went.
pub fn overhang(size: f32, peak: f32) -> (f32, f32) {
    let over = ((peak - 1.0) * size).max(0.0);
    (over / 2.0, over)
}

/// How far a tile `near` the pointer swells (DIPs from its middle): wholly on it, not at all a `step` away, the cosine
/// between. Bell-shaped, and flat where two tiles' reaches meet, so a row under the hand has no crease in it — a
/// straight taper left a corner there, and a tile read as swelling in steps as the pointer crossed it.
pub fn swell(near: f32, step: f32) -> f32 {
    if step <= 0.0 || near >= step {
        0.0
    } else {
        0.5 * (1.0 + (std::f32::consts::PI * (near / step)).cos())
    }
}

/// The row as `entries` ask, at rest or with the pointer `pointer` along it (DIPs from the glass's left; none: the
/// row as it lies). A gap takes room in the row and swells nothing. The glass is as large as the row lies: a tile
/// swelled is drawn beyond it (see the top), not accommodated by growing it.
pub fn row(entries: &[Entry], pointer: Option<f32>, size: f32, spacing: f32, peak: f32) -> Row {
    let step = pitch(size, spacing);
    let mut tiles = Vec::with_capacity(entries.len());
    let mut after = PAD;
    for entry in entries {
        let side = if entry.gap() { 0.0 } else { size };
        let centre = after + side / 2.0;
        let scale = if entry.gap() {
            1.0
        } else {
            1.0 + (peak - 1.0) * pointer.map_or(0.0, |at| swell((at - centre).abs(), step))
        };
        tiles.push(Tile { centre, scale });
        after = centre + side / 2.0 + spacing;
    }
    // (The last one's room before it is counted once: the row ends where its tiles do.)
    let length = if entries.is_empty() {
        2.0 * PAD
    } else {
        after - spacing + PAD
    };
    Row {
        tiles,
        length,
        thickness: 2.0 * PAD + size,
    }
}

/// The same row with every tile's swell brought `share` of the way from rest (0: as it lies, 1: as the pointer has
/// it). The glass is as it was measured, so what moves as the swell eases is the tiles alone: the dock comes up to
/// the pointer and settles back as the pointer leaves, without its window changing size under it.
pub fn eased(row: &Row, share: f32) -> Row {
    if (share - 1.0).abs() < 0.001 {
        return row.clone();
    }
    Row {
        tiles: row
            .tiles
            .iter()
            .map(|tile| Tile {
                centre: tile.centre,
                scale: 1.0 + (tile.scale - 1.0) * share,
            })
            .collect(),
        length: row.length,
        thickness: row.thickness,
    }
}

/// The glass's corners: as round as it may be, its ends pill-shaped while it is narrow.
pub fn radius(thickness: f32) -> f32 {
    RADIUS.min(thickness / 2.0)
}

/// The box a tile is drawn in, in the glass (DIPs from its top left): the same box a press is judged against, so what
/// is seen is what is hit. Tiles stand on the glass's floor under a dock along the foot of the screen, and hang from
/// its underside along the top, so the row swells away from the screen's edge.
pub fn box_of(tile: Tile, size: f32, thickness: f32, at_top: bool) -> (f32, f32, f32, f32) {
    let side = size * tile.scale;
    let y = if at_top { PAD } else { thickness - PAD - side };
    (tile.centre - side / 2.0, y, side, side)
}

/// Which tile is drawn in which order: the least swollen first, the most last, over the rest; among two alike, the
/// further along the row last, so a swell leans forward rather than back.
pub fn order(tiles: &[Tile]) -> Vec<usize> {
    let mut by: Vec<usize> = (0..tiles.len()).collect();
    by.sort_by(|a, b| {
        tiles[*a]
            .scale
            .partial_cmp(&tiles[*b].scale)
            .unwrap_or(Ordering::Equal)
            .then(a.cmp(b))
    });
    by
}

/// Which tile a point of the glass is on (DIPs from its top left): the last of those drawn over it. None where the
/// point is on a gap, or past the row's ends. Where tiles overlap as they swell, the one that covers the point and is
/// drawn over the rest has the press — which is the one that is seen there.
pub fn hit(
    tiles: &[Tile],
    size: f32,
    thickness: f32,
    at_top: bool,
    at: (f32, f32),
) -> Option<usize> {
    order(tiles).into_iter().rev().find(|&i| {
        let (x, y, width, height) = box_of(tiles[i], size, thickness, at_top);
        width > 0.0 && at.0 >= x && at.0 < x + width && at.1 >= y && at.1 < y + height
    })
}

/// The shadow the glass casts, its corner at the canvas's: outside the glass only, `MARGIN` around it at most.
pub fn shadow(frame: &dyn Canvas, row: &Row, glass: Glass, dragged: bool) {
    let (glass, palette) = shown(glass, dragged);
    for (color, blur, drop) in palette.shadows {
        frame.shadow(
            color.alpha(glass.presence),
            0.0,
            0.0,
            row.length,
            row.thickness,
            radius(row.thickness),
            blur,
            drop,
        );
    }
}

/// How the glass is drawn, and in which colours: as it is, or as it is in hand.
fn shown(glass: Glass, dragged: bool) -> (Glass, &'static Palette) {
    let glass = if dragged { DRAGGED } else { glass };
    (glass, colours(glass.tone))
}

/// Draws the glass and its tiles from the canvas's corner: its tint (over what the window draws behind it), its rim,
/// and the tiles on it, at `px` physical pixels a DIP. `over`: the tile the pointer is on, given a plate to stand
/// on; dragged, the glass is in the colour that says it is being moved.
#[allow(clippy::too_many_arguments)]
pub fn paint(
    frame: &dyn Canvas,
    entries: &[Entry],
    row: &Row,
    glass: Glass,
    size: f32,
    at_top: bool,
    over: Option<usize>,
    dragged: bool,
    px: f32,
) {
    let (glass, palette) = shown(glass, dragged);
    let presence = glass.presence;
    let (length, thickness) = (row.length, row.thickness);
    let corner = radius(thickness);
    let tint = if dragged {
        Color::hex(DRAGGED_TINT, glass.opacity * presence)
    } else {
        Color::hex(palette.tint, glass.opacity * presence)
    };
    frame.fill_rounded(tint, 0.0, 0.0, length, thickness, corner);
    // The tiles, least swollen first: what the pointer is on drawn last, over its neighbours.
    let drawn = order(&row.tiles);
    for &index in &drawn {
        let tile = row.tiles[index];
        let entry = &entries[index];
        let (x, y, width, height) = box_of(tile, size, thickness, at_top);
        // (A gap is known by what it is, not by its box: `row` gives it a scale of 1.0 so it never swells, so its
        // box is a tile's wide even though it takes no room in the row — width would never read 0 here.)
        if entry.gap() {
            // A gap: a divider down the middle of its room, centred on the tiles and most of a tile tall — made to be
            // seen, not the hairline it was (its colour is raised in the palettes, and kept solid as the glass fades).
            let deep = size * 0.6;
            let (from, to) = (thickness / 2.0 - deep / 2.0, thickness / 2.0 + deep / 2.0);
            let wide = 2.0 / px;
            frame.fill(
                palette.faint.alpha(presence.max(0.85)),
                tile.centre - wide / 2.0,
                from,
                wide,
                to - from,
            );
            continue;
        }
        let under = over == Some(index);
        if under {
            // (A plate a size larger than the tile, so a picture's own margins are inside it.)
            let room = width + 8.0;
            frame.fill_rounded(
                palette.tile.alpha(0.9 * presence.max(0.35)),
                tile.centre - room / 2.0,
                y - 4.0,
                room,
                room,
                room * 0.24,
            );
        }
        match &entry.picture {
            Some(picture) => frame.picture(picture, x, y, width, height, width * TILE_RADIUS, 1.0),
            None => {
                // A door stands on its own coloured plate with the mark drawn light on top, as macOS gives every app a
                // colour; the Bin and what is kept keep the faint plate and the ink mark.
                let (plate, glyph) = match entry.badge() {
                    Some(badge) => (badge.alpha(presence), Color::hex(0xFFFFFF, 0.96)),
                    None => (palette.tile.alpha(presence), palette.ink),
                };
                frame.fill_rounded(plate, x, y, width, height, width * 0.24);
                let side = width * MARK;
                if let Some(mark) = entry.mark() {
                    frame.icon(
                        mark,
                        Point {
                            x: tile.centre,
                            y: y + width / 2.0,
                        },
                        side,
                        glyph,
                    );
                }
            }
        }
    }
    // What is running, over the tiles: the dots are never covered, so the row tells it at a glance however far a
    // neighbour has swelled.
    for (index, entry) in entries.iter().enumerate() {
        if !entry.running || entry.gap() {
            continue;
        }
        let colour = palette.dot.alpha(presence.max(0.6));
        let y = if at_top { DOT_IN } else { thickness - DOT_IN };
        frame.fill_circle(
            colour,
            Point {
                x: row.tiles[index].centre,
                y,
            },
            DOT,
        );
    }
    // The rim: one pixel inside the glass's edge, lit from above.
    let line = 1.0 / px;
    let rim = Fill::Down {
        top: 0.0,
        from: palette.rim.0.alpha(presence),
        bottom: thickness,
        to: palette.rim.1.alpha(presence),
    };
    frame.stroke_rounded(rim, 0.0, 0.0, length, thickness, corner, line);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(kinds: &[char]) -> Vec<Entry> {
        kinds
            .iter()
            .map(|kind| Entry {
                act: match kind {
                    'g' => Act::Gap,
                    'd' => Act::Door(DockDoor::Settings),
                    'w' => Act::GoTo(format!("c:\\{kind}.exe")),
                    _ => Act::Launch(format!("c:\\{kind}.exe")),
                },
                name: kind.to_string(),
                picture: if *kind == 'g' || *kind == 'd' {
                    None
                } else {
                    Some(Picture {
                        key: 1,
                        size: (32, 32),
                        pixels: Default::default(),
                    })
                },
                running: *kind == 'r' || *kind == 'w',
            })
            .collect()
    }

    #[test]
    fn a_tile_at_full_peak_overhangs_the_glass_as_far_as_said() {
        let (size, spacing, peak) = (46.0, 8.0, 1.5);
        let (across, band) = overhang(size, peak);
        let made = entries(&['a', 'b', 'c']);
        let rest = row(&made, None, size, spacing, peak);
        // The first tile with the pointer on it, swelled all it can: it stands where it did, and reaches as far past
        // the glass's end and its edge as the room the window is shaped to take the pointer over.
        let first = row(&made, Some(rest.tiles[0].centre), size, spacing, peak);
        let (x, y, width, height) = box_of(first.tiles[0], size, first.thickness, false);
        assert_eq!(width, size * peak);
        assert!((x + across - PAD).abs() < 1e-3, "{x}");
        assert!((y + band - PAD).abs() < 1e-3, "{y}");
        assert!(
            (y + height - (first.thickness - PAD)).abs() < 1e-3,
            "{y} {height}"
        );
        // The same at the other end, where it overhangs outward instead.
        let last = made.len() - 1;
        let tail = row(&made, Some(rest.tiles[last].centre), size, spacing, peak);
        let (right, _, width, _) = box_of(tail.tiles[last], size, tail.thickness, false);
        assert!(
            (right + width - (tail.length + across - PAD)).abs() < 1e-3,
            "{right} {width} {}",
            tail.length
        );
    }

    #[test]
    fn nothing_reaches_past_the_glass_further_than_the_window_reaches_for_it() {
        let (size, spacing, peak) = (46.0, 8.0, 1.9);
        let (across, band) = overhang(size, peak);
        for kinds in [
            &['a'][..],
            &['a', 'b', 'c', 'd', 'e'][..],
            &['a', 'g', 'b'][..],
            &[][..],
        ] {
            let made = entries(kinds);
            for pointer in [None, Some(0.0), Some(120.0), Some(1e4)] {
                for at_top in [false, true] {
                    let row = row(&made, pointer, size, spacing, peak);
                    for tile in &row.tiles {
                        let (x, y, w, h) = box_of(*tile, size, row.thickness, at_top);
                        assert!(
                            x >= -across - 1e-3 && x + w <= row.length + across + 1e-3,
                            "{kinds:?} {pointer:?} {tile:?} {row:?}"
                        );
                        assert!(
                            y >= -band - 1e-3 && y + h <= row.thickness + band + 1e-3,
                            "{kinds:?} {pointer:?} {tile:?} {row:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_swell_reaches_one_neighbour_and_is_smooth_where_their_reaches_meet() {
        let step = pitch(46.0, 8.0);
        assert_eq!(swell(0.0, step), 1.0);
        assert_eq!(swell(step, step), 0.0);
        assert_eq!(swell(step * 2.0, step), 0.0);
        // Halfway between two tiles' middles each has the same reach, and neither stops short: a row of swells has no
        // crease where one hands over to the next.
        let half = swell(step / 2.0, step);
        assert!((half - 0.5).abs() < 1e-6, "{half}");
        let about = |at: f32| swell(at, step) + swell(step - at, step);
        for i in 0..=100 {
            let at = step * i as f32 / 100.0;
            assert!((about(at) - about(at + step / 100.0)).abs() < 0.06, "{at}");
        }
        // A gap swells nothing, whatever is set to swell.
        let with = row(&entries(&['a', 'g', 'b']), Some(300.0), 46.0, 8.0, 1.5);
        assert_eq!(with.tiles[1].scale, 1.0);
    }

    #[test]
    fn the_glass_is_as_large_whatever_the_pointer_does_to_the_tiles_in_it() {
        let (size, spacing, peak) = (46.0, 8.0, 1.5);
        let made = entries(&['a', 'b', 'c']);
        // The row's own length: two ends' padding, three tiles, two rooms between them. The glass is that and no
        // more, and the same with the pointer on it as off it: the slots the tiles stand in are constants, and a
        // window that grew under the hand would move them out from under it.
        let rest = row(&made, None, size, spacing, peak);
        assert_eq!(rest.length, 2.0 * PAD + 3.0 * size + 2.0 * spacing);
        assert_eq!(rest.thickness, 2.0 * PAD + size);
        let stands = |row: &Row| row.tiles.iter().map(|tile| tile.centre).collect::<Vec<_>>();
        for pointer in [
            None,
            Some(-1e3),
            Some(rest.tiles[1].centre),
            Some(rest.length + 1e3),
        ] {
            let drawn = row(&made, pointer, size, spacing, peak);
            assert_eq!(
                (drawn.length, drawn.thickness),
                (rest.length, rest.thickness),
                "{pointer:?}"
            );
            assert_eq!(stands(&drawn), stands(&rest), "{pointer:?}");
        }
        // Nor is it larger for the peak it is drawn to: that is room outside it, not inside.
        let wider = row(&made, None, size, spacing, 2.2);
        assert_eq!(
            (wider.length, wider.thickness),
            (rest.length, rest.thickness)
        );
        // Tiles stand on the glass's floor under a dock along the foot of the screen, and hang from its underside
        // along the top: what swells leaves the glass by whichever edge is further from the screen's.
        for (index, tile) in rest.tiles.iter().enumerate() {
            let (_, bottom, _, height) = box_of(*tile, size, rest.thickness, false);
            assert!((bottom + height - (rest.thickness - PAD)).abs() < 1e-4);
            let (_, top, _, _) = box_of(*tile, size, rest.thickness, true);
            assert!((top - PAD).abs() < 1e-4, "{index}");
        }
    }

    #[test]
    fn the_swell_eased_back_leaves_the_glass_where_it_was() {
        let (size, spacing, peak) = (46.0, 8.0, 1.5);
        let made = entries(&['a', 'b', 'c']);
        let rest = row(&made, None, size, spacing, peak);
        let swollen = row(&made, Some(rest.tiles[1].centre), size, spacing, peak);
        // Brought none of the way it is the row as it lies, and all of the way as the pointer has it: the middle
        // of the two is the same tiles less swelled, in the same places, in the same glass.
        assert_eq!(eased(&swollen, 0.0), rest);
        assert_eq!(eased(&swollen, 1.0), swollen);
        let half = eased(&swollen, 0.5);
        assert_eq!(
            (half.length, half.thickness),
            (swollen.length, swollen.thickness)
        );
        for (tile, (at, rest)) in half
            .tiles
            .iter()
            .zip(swollen.tiles.iter().zip(rest.tiles.iter()))
        {
            assert_eq!(tile.centre, at.centre);
            assert!(
                (tile.scale - 1.0 - (at.scale - 1.0) * 0.5).abs() < 1e-5,
                "{tile:?} {at:?}"
            );
            assert!(tile.scale >= rest.scale - 1e-5, "{tile:?} {rest:?}");
        }
    }

    #[test]
    fn a_press_goes_to_the_tile_that_is_seen_there() {
        let made = entries(&['a', 'b', 'c']);
        let swollen = row(&made, Some(120.0), 46.0, 8.0, 1.5);
        // Where two overlap, the more swollen is drawn over the less, and has the press: the last drawn is the
        // largest of them.
        let over = order(&swollen.tiles);
        let biggest = (0..swollen.tiles.len())
            .max_by_key(|&i| (swollen.tiles[i].scale * 1e6) as i64)
            .unwrap();
        assert_eq!(over.last().copied(), Some(biggest), "{over:?} {swollen:?}");
        for point in 0..200 {
            let at = (point as f32, swollen.thickness - PAD);
            if let Some(won) = hit(&swollen.tiles, 46.0, swollen.thickness, false, at) {
                // The tile it belongs to is the one drawn there, over all the rest that cover the point too.
                let drawn = order(&swollen.tiles).into_iter().rev().filter(|&i| {
                    let (x, y, w, h) = box_of(swollen.tiles[i], 46.0, swollen.thickness, false);
                    at.0 >= x && at.0 < x + w && at.1 >= y && at.1 < y + h
                });
                assert_eq!(drawn.last(), Some(won), "{at:?} {swollen:?}");
            }
        }
        // A gap is room, not a tile: nothing lies on it.
        let gapped = row(&entries(&['a', 'g', 'b']), None, 46.0, 8.0, 1.0);
        let middle = gapped.tiles[1].centre;
        assert_eq!(
            hit(
                &gapped.tiles,
                46.0,
                gapped.thickness,
                false,
                (middle, gapped.thickness - PAD)
            ),
            None
        );
    }

    #[test]
    fn nothing_is_there_without_a_picture_or_a_mark() {
        let made = entries(&['a', 'g', 'd', 'w']);
        // A thing kept at hand is pictured, with a mark to fall back on where the shell gives none.
        assert!(made[0].picture.is_some() && made[0].mark().is_some());
        // A gap is room alone: nothing to draw, nothing to press.
        assert!(made[1].gap() && made[1].mark().is_none() && made[1].picture.is_none());
        // A door is drawn with its mark on a plate, and opens nothing of the shell's.
        assert!(
            made[2].picture.is_none() && made[2].mark().is_some() && made[2].target().is_none()
        );
        // What is running has a target to go to, though it is kept nowhere in the settings.
        assert_eq!(made[3].target(), Some("c:\\w.exe"));
        assert!(made[3].running);
        assert_eq!(made[0].target(), Some("c:\\a.exe"));
    }
}
