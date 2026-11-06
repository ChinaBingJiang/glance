//! The launchpad's shape: a screen of tiles, page after page, the place at its head where its search is written in,
//! and the folders its tiles are merged into. No window, no pointer, nothing running — only where its tiles lie and how
//! they are drawn, as the dock's shape is (see `crate::ui::dock`); which programs there are, and what the pointer and
//! the keys do to them, are the launchpad's own (see `crate::launchpad`).
//!
//! The grid is chosen for the screen it is laid out on and the tiles it has to hold: of the grids that hold them all on
//! one page, the one with the biggest tiles, and where none can, the one that needs the fewest pages. A tile stands in
//! the middle of a cell about twice as wide as itself, which is what leaves the air between the tiles that a Mac's
//! launchpad has; a tile's name is written in the cell's floor, so the rows stand farther apart than the columns.

use super::canvas::{Align, Canvas, Color, Family, Fill, Font, Picture, Point};
use super::icons::Icon;

/// One of them: its name, what opens it, its picture where the shell has given one, and whether the dock keeps it.
/// A folder of them says what stands in it, and is drawn from their pictures.
#[derive(Clone, PartialEq, Debug)]
pub struct Tile {
    pub name: String,
    pub target: String,
    pub picture: Option<Picture>,
    pub kept: bool,
    /// What stands in this tile where it is a folder of them: none where it is one program.
    pub group: Vec<Tile>,
}

impl Tile {
    /// Whether this tile is a folder of others.
    pub fn is_group(&self) -> bool {
        !self.group.is_empty()
    }

    /// What is drawn in a tile whose picture has not come yet: the first letter its name says.
    pub fn letter(&self) -> char {
        self.name
            .trim()
            .chars()
            .find(|one| !one.is_whitespace())
            .map(|one| one.to_uppercase().next().unwrap_or(one))
            .unwrap_or('?')
    }
}

/// How the tiles lie: as many across and down as the screen takes, at the biggest side the room gives them with room
/// to breathe between, and into just so many pages.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    /// A tile's picture's side (DIPs).
    pub side: f32,
    /// A cell's own width and height: the room shared out by the columns and the rows, so the grid reaches the room's
    /// ends whatever they are. A tile stands in its cell's middle across, and at its top down, where its name is.
    pub pitch: (f32, f32),
    /// Where the room's own corner is: the first cell's, whose middle across its first picture stands in.
    pub from: (f32, f32),
    /// What one page holds.
    pub per: usize,
}

/// The least and most that may go across and down, and a tile's own least and biggest side (DIPs). A Mac keeps seven
/// of a page's eight columns and five of its rows for the fullest screen, so these are the bounds it is worked between.
const ACROSS: (usize, usize) = (5, 7);
const DOWN: (usize, usize) = (3, 5);
const SIDE: (f32, f32) = (56.0, 116.0);
/// How small a tile an asked-for grid may make (DIPs): past this, denser than a picture and its name can be read at,
/// and the screen's own choice takes over again (see `Grid::asked`). Looser than a screen's own least, since here
/// the keeper of the screen asked for it.
const ASKED: f32 = 40.0;
/// The least a cell may be of the picture it holds: across, and down (see `lie`, where what a cell leaves besides the
/// picture is the air between neighbours — come at out of the room and the rows rather than set beforehand). The looser
/// the down one, the smaller the tiles and the more room between their rows.
const AIR: (f32, f32) = (1.3, 1.5);
/// The room a name's letters need under its picture besides the air between the rows (DIPs), taken off a cell before
/// its rows are counted out.
const NAME: f32 = 12.0;
/// A folder's own grid: how many columns its tiles may make at the most. Standing in a folder the tiles are laid for
/// themselves — as many columns as they need, up to this, as many rows as they fill, and the block of them stood in the
/// room's middle rather than stretched over its ends. Their side is the whole screen's page's own, so stepping into a
/// folder or out of it leaves the pictures asked for at the size they were fetched at already.
const IN_ACROSS: usize = 8;
/// A folder's tile is a plate with nine of its pictures inset in it: how much of its side the plate keeps back at its
/// edge, and how much again of what is left over is lined off before the nine begin.
const SHELL: f32 = 0.12;
const LINING: f32 = 0.08;
/// The room left at the head for the search and at the foot for the pages, and at the screen's sides (DIPs).
const HEAD: f32 = 128.0;
const FOOT: f32 = 58.0;
const MARGIN: f32 = 72.0;
/// How high a tile's name's letters are, at most and least (DIPs).
const WORD: (f32, f32) = (11.5, 16.0);
/// The pages' dots: how far apart and how big (DIPs), and how near the point must be to be one.
const STEP: f32 = 18.0;
const DOT: f32 = 4.5;
/// How far a tile's corners are rounded, as its side: a program's picture is a rounded square, and a folder's plate
/// is drawn the same to stand among them.
const CORNER: f32 = 0.225;
/// A folder's name at the head of its own page, where the search's box stands on the whole screen: a title, not a box
/// to be searched in — its letters, the band it is clicked and written in, and the room kept inside that band for the
/// letters being written (DIPs).
const TITLE: f32 = 19.0;
const TITLE_HEFT: f32 = 600.0;
const TITLE_WIDE: (f32, f32) = (0.46, 460.0);
const TITLE_INSET: f32 = 26.0;

impl Grid {
    /// How `count` tiles lie on a screen `wide` × `tall` DIPs: of the grids that hold them all on one page, the one
    /// with the biggest tiles; where none can, the one with room for the most on a page, and so the fewest pages.
    pub fn of(wide: f32, tall: f32, count: usize) -> Grid {
        let room = room(wide, tall);
        let mut best: Option<Grid> = None;
        for rows in DOWN.0..=DOWN.1 {
            for cols in ACROSS.0..=ACROSS.1 {
                // (A grid whose tiles would be too small to know is no grid at all.)
                let grid = lie(room, cols, rows);
                if grid.side < SIDE.0 {
                    continue;
                }
                if best.is_none_or(|was| serves(&grid, &was, count)) {
                    best = Some(grid);
                }
            }
        }
        // (A screen taking none of them: the least that can be drawn, and the pages will have to do.)
        best.unwrap_or_else(|| lie(room, ACROSS.0, DOWN.0))
    }

    /// How many pages `count` tiles make.
    pub fn pages(&self, count: usize) -> usize {
        (count.max(1) + self.per - 1) / self.per
    }

    /// How `count` tiles lie where the settings have asked for `cols` across and `rows` down: the room is drawn between
    /// its corners first, the tiles as big and as far apart as that room and the grid together allow — the scale follows
    /// the grid, not the other way about. A 0 of either is the screen's own choice (see `of`), and so is a request
    /// denser than tiles can be read at (see `ASKED` and `crate::settings::DockSettings::pad_cols`). Asks as small as
    /// one across and one down are taken for what they say: however the settings are set, each stop of them means the
    /// page it names.
    pub fn asked(wide: f32, tall: f32, count: usize, cols: usize, rows: usize) -> Grid {
        if cols == 0 || rows == 0 {
            return Grid::of(wide, tall, count);
        }
        let (cols, rows) = (cols.clamp(1, 12), rows.clamp(1, 10));
        let grid = lie(room(wide, tall), cols, rows);
        if grid.side < ASKED {
            return Grid::of(wide, tall, count);
        }
        grid
    }

    /// How a folder's own tiles lie on a screen `wide` × `tall` DIPs, at the side `asked` — the side the whole
    /// screen's page gives its tiles. As many columns as they need, up to `IN_ACROSS`, and as many rows as they fill,
    /// up to what the room takes at that side; the block of them stood in the room's middle, its cells measured from
    /// its tiles rather than stretched over the room's ends. The settings' page density is not asked here, save as it
    /// settled `asked`.
    pub fn inside(wide: f32, tall: f32, count: usize, asked: f32) -> Grid {
        let room = room(wide, tall);
        let asked = asked.clamp(ASKED, SIDE.1);
        let cell = (asked * AIR.0, asked * AIR.1 + NAME);
        // (However many the room takes at that side: a folder of three is three across, and a folder of a hundred is
        // as many across as it may be and no more.)
        let (across, down) = (
            (room.0 / cell.0).floor().max(1.0) as usize,
            (room.1 / cell.1).floor().max(1.0) as usize,
        );
        let cols = count.clamp(1, IN_ACROSS).min(across);
        let rows = count.div_ceil(cols).clamp(1, down);
        let side = asked
            .min((room.0 / cols as f32) / AIR.0)
            .min(((room.1 / rows as f32) - NAME) / AIR.1)
            .clamp(ASKED, SIDE.1);
        let pitch = (side * AIR.0, side * AIR.1 + NAME);
        let block = (cols as f32 * pitch.0, rows as f32 * pitch.1);
        Grid {
            cols,
            rows,
            side,
            pitch,
            from: (
                MARGIN + ((room.0 - block.0) / 2.0).max(0.0),
                HEAD + ((room.1 - block.1) / 2.0).max(0.0),
            ),
            per: cols * rows,
        }
    }

    /// Where the `index`th tile of a page is: its picture's box, centred in its cell across and standing at the cell's
    /// top down, where its name goes beneath it.
    pub fn cell(&self, index: usize) -> (f32, f32, f32, f32) {
        let (col, row) = (index % self.cols, index / self.cols);
        (
            self.from.0 + col as f32 * self.pitch.0 + (self.pitch.0 - self.side) / 2.0,
            self.from.1 + row as f32 * self.pitch.1,
            self.side,
            self.side,
        )
    }

    /// Which tile of a page of `on` of them the point is on: none off the grid, or past its last tile. The cell is the
    /// tile's to answer to, its air and its name and all: the hand is not judged to the pixel.
    pub fn hit(&self, on: usize, x: f32, y: f32) -> Option<usize> {
        let col = ((x - self.from.0) / self.pitch.0).floor();
        let row = ((y - self.from.1) / self.pitch.1).floor();
        if col < 0.0 || row < 0.0 {
            return None;
        }
        let (col, row) = (col as usize, row as usize);
        if col >= self.cols || row >= self.rows {
            return None;
        }
        let index = row * self.cols + col;
        (index < on).then_some(index)
    }
}

/// The room the tiles are laid in on a screen `wide` × `tall` DIPs: between the search at its head and the pages' dots
/// at its foot, clear of the screen's own sides. Its corners are settled before a tile's size is asked for, so the
/// rows and the columns have the last word on the tiles and on the air between them (see `lie`).
fn room(wide: f32, tall: f32) -> (f32, f32) {
    (wide - 2.0 * MARGIN, tall - HEAD - FOOT)
}

/// Whether `new` suits `count` tiles better than `was` does: what holds them all beats what does not, and between
/// those that hold them the bigger tiles win — between those that cannot, the more on a page, and then the bigger
/// tiles.
fn serves(new: &Grid, was: &Grid, count: usize) -> bool {
    let (holds, held) = (new.per >= count, was.per >= count);
    match (holds, held) {
        (true, true) => {
            (new.side, std::cmp::Reverse(new.per)) > (was.side, std::cmp::Reverse(was.per))
        }
        (false, false) => (new.per, new.side) > (was.per, was.side),
        (true, false) => true,
        (false, true) => false,
    }
}

/// A grid of `cols` × `rows` tiles in `room`: the room's own corners are settled first (see `room`), and it is then
/// shared out by the columns and the rows, so the cells fill it from end to end whichever way the grid goes. A
/// picture is as big as its cell allows — `AIR` of a cell across and down at the least, its name's room taken off the
/// cell down first — and never past `SIDE.1`; what the cell has over is the gap beside and under it, which is come at
/// rather than set. The first cell stands at the room's corner and the pictures are centred in their cells, so the
/// page reads from its head down and reaches its foot, rather than floating in the middle of what it left over.
fn lie(room: (f32, f32), cols: usize, rows: usize) -> Grid {
    let pitch = (room.0 / cols as f32, room.1 / rows as f32);
    let side = (pitch.0 / AIR.0)
        .min((pitch.1 - NAME) / AIR.1)
        .min(SIDE.1)
        .max(0.0);
    Grid {
        cols,
        rows,
        side,
        pitch,
        from: (MARGIN, HEAD),
        per: cols * rows,
    }
}

/// What the pointer is doing to the page: the tile it stands on, the one its button has taken up and where it carries
/// it (to merge them into a folder), and how far the page has come in, which is how far its tiles are washed up from
/// nothing (see `crate::ui::life`).
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Hand {
    pub over: Option<usize>,
    pub held: Option<usize>,
    pub at: (f32, f32),
    pub into: Option<usize>,
    pub come: f32,
    /// The tile the keys have picked — highlighted with a ring, where the pointer's plate only stands under it.
    pub picked: Option<usize>,
}

/// How the screen is washed, and what is drawn on it.
struct Sheet {
    text: Color,
    faint: Color,
    plate: Color,
    tile: Color,
    dot: Color,
    lit: Color,
}

fn sheet(dark: bool) -> Sheet {
    if dark {
        Sheet {
            text: Color::hex(0xF2F5FA, 0.94),
            faint: Color::hex(0xF2F5FA, 0.6),
            plate: Color::hex(0xFFFFFF, 0.14),
            tile: Color::hex(0xFFFFFF, 0.1),
            dot: Color::hex(0xFFFFFF, 0.5),
            lit: Color::hex(0xFFFFFF, 0.92),
        }
    } else {
        Sheet {
            text: Color::hex(0x1A1F27, 0.94),
            faint: Color::hex(0x1A1F27, 0.6),
            plate: Color::hex(0xFFFFFF, 0.72),
            tile: Color::hex(0xFFFFFF, 0.6),
            dot: Color::hex(0x1A1F27, 0.45),
            lit: Color::hex(0x1A1F27, 0.88),
        }
    }
}

/// `text` with its middle on `middle`, in a box `wide` across whose top is `top`: `Align` begins and ends, and has no
/// middle, so it is the text's own width that has to put it in the middle.
fn centred(
    frame: &dyn Canvas,
    text: &str,
    font: Font,
    colour: Color,
    middle: f32,
    top: f32,
    wide: f32,
) {
    let was = frame.measure(text, font).min(wide);
    frame.text(
        text,
        font,
        colour,
        middle - was / 2.0,
        top,
        wide,
        Align::Start,
    );
}

/// A folder's tile, drawn from its plate: the side of the nine small pictures inset in it, and where each of their
/// corners stands from the tile's own. The plate keeps `SHELL` of its side at its edge and `LINING` of what is left
/// over again before the nine begin, with a hair of gap between them — so a folder of one and a folder of ninety
/// keep the same outer edge and the same picture size.
fn nine(side: f32) -> (f32, Vec<(f32, f32)>) {
    let inner = side * (1.0 - 2.0 * SHELL) * (1.0 - 2.0 * LINING);
    let gap = (inner * 0.02).max(1.0);
    let tile = (inner - 2.0 * gap) / 3.0;
    let near = (side - inner) / 2.0;
    (
        tile,
        (0..9)
            .map(|at| {
                (
                    near + (at % 3) as f32 * (tile + gap),
                    near + (at / 3) as f32 * (tile + gap),
                )
            })
            .collect(),
    )
}

/// One tile, drawn with its middle at `middle`, as wide as `side`, in a cell `cell` wide: its picture, or the plate
/// and letters of what has none, or, for a folder, its own plate with what stands in it. `fade` is how far it has
/// come in.
fn one(
    frame: &dyn Canvas,
    tile: &Tile,
    middle: Point,
    side: f32,
    cell: f32,
    colours: &Sheet,
    name: Option<Font>,
    fade: f32,
) {
    let (x, y) = (middle.x - side / 2.0, middle.y - side / 2.0);
    let corner = side * CORNER;
    if tile.is_group() {
        frame.fill_rounded(colours.plate.alpha(fade), x, y, side, side, corner);
        let (room, places) = nine(side);
        for (at, (left, above)) in places.iter().enumerate() {
            let (left, above) = (x + left, y + above);
            match tile.group.get(at) {
                Some(one) => match &one.picture {
                    Some(picture) => {
                        frame.picture(picture, left, above, room, room, room * 0.2, fade)
                    }
                    None => {
                        frame.fill_rounded(
                            colours.tile.alpha(fade),
                            left,
                            above,
                            room,
                            room,
                            room * 0.2,
                        );
                        let letter = one.letter().to_string();
                        centred(
                            frame,
                            &letter,
                            Font::new(Family::Segoe, room * 0.5, 600.0),
                            colours.text.alpha(fade),
                            left + room / 2.0,
                            above + room * 0.24,
                            room,
                        );
                    }
                },
                // (Past the eighth of them: the last of the nine says how many more stand in the folder.)
                None if at == 8 && tile.group.len() > 8 => {
                    let more = format!("+{}", tile.group.len() - 8);
                    centred(
                        frame,
                        &more,
                        Font::new(Family::Segoe, room * 0.46, 600.0),
                        colours.text.alpha(fade),
                        left + room / 2.0,
                        above + room * 0.27,
                        room,
                    );
                }
                None => {}
            }
        }
    } else {
        match &tile.picture {
            Some(picture) => frame.picture(picture, x, y, side, side, corner, fade),
            None => {
                frame.fill_rounded(colours.tile.alpha(fade), x, y, side, side, corner);
                let letter = tile.letter().to_string();
                centred(
                    frame,
                    &letter,
                    Font::new(Family::Segoe, side * 0.42, 600.0),
                    colours.text.alpha(fade),
                    middle.x,
                    y + side * 0.26,
                    side,
                );
            }
        }
        if tile.kept {
            let mark = (side * 0.19).clamp(11.0, 17.0);
            frame.icon(
                Icon::Pin,
                Point {
                    x: x + mark * 1.1,
                    y: y + mark,
                },
                mark,
                colours.lit.alpha(fade),
            );
        }
    }
    // (Its name under it, cut short with an ellipsis: as wide as its cell, which is wider than the tile.)
    if let Some(letters) = name {
        centred(
            frame,
            &tile.name,
            letters,
            colours.text.alpha(fade),
            middle.x,
            y + side + side * 0.1,
            cell - 10.0,
        );
    }
}

/// A page of tiles, drawn from the canvas's corner: `on` of them, `tiles` from `from` on. What the pointer stands on is
/// given a plate to stand on; a tile taken up is drawn last of all, under the pointer, over the one it would join.
pub fn paint(
    frame: &dyn Canvas,
    tiles: &[Tile],
    grid: &Grid,
    from: usize,
    on: usize,
    hand: Hand,
    dark: bool,
) {
    let colours = sheet(dark);
    let name = Font::new(
        Family::Segoe,
        (grid.side * 0.16).clamp(WORD.0, WORD.1),
        400.0,
    );
    let fade = hand.come.clamp(0.0, 1.0);
    for index in 0..on {
        if hand.held == Some(index) {
            continue;
        }
        let tile = &tiles[from + index];
        let (x, y, side, _) = grid.cell(index);
        let middle = Point {
            x: x + side / 2.0,
            y: y + side / 2.0,
        };
        if hand.over == Some(index) || hand.into == Some(index) {
            let room = side * 1.18;
            frame.fill_rounded(
                colours
                    .plate
                    .alpha(fade * if hand.into == Some(index) { 1.0 } else { 0.7 }),
                middle.x - room / 2.0,
                middle.y - room / 2.0,
                room,
                room,
                room * CORNER,
            );
        }
        // (The keys' pick: a ring round its cell, kept over the pointer's plate where both fall on one tile.)
        if hand.picked == Some(index) {
            let room = side * 1.3;
            frame.stroke_rounded(
                Fill::Down {
                    top: middle.y - room / 2.0,
                    from: colours.lit.alpha(0.85 * fade),
                    bottom: middle.y + room / 2.0,
                    to: colours.lit.alpha(0.4 * fade),
                },
                middle.x - room / 2.0,
                middle.y - room / 2.0,
                room,
                room,
                room * CORNER,
                2.0,
            );
        }
        one(
            frame,
            tile,
            middle,
            side,
            grid.pitch.0,
            &colours,
            Some(name),
            fade,
        );
    }
    // (The one the pointer carries, and the name it carries under it: what is held is drawn over the page, as big as
    // the page's tiles and a little larger, where the hand has brought it.)
    if let Some(tile) = hand.held.and_then(|at| tiles.get(from + at)) {
        let side = grid.side * 1.12;
        one(
            frame,
            tile,
            Point {
                x: hand.at.0,
                y: hand.at.1,
            },
            side,
            grid.pitch.0,
            &colours,
            None,
            1.0,
        );
        centred(
            frame,
            &tile.name,
            name,
            colours.text,
            hand.at.0,
            hand.at.1 + side / 2.0 + 8.0,
            grid.pitch.0.max(side),
        );
    }
}

/// Where the pages' dots stand by default: their height at the screen's foot.
pub fn feet(tall: f32) -> f32 {
    tall - FOOT / 2.0 - 4.0
}

/// How tall a screen the grid may keep where the dots' row stands at `dots`: the tiles end their rows, names and all,
/// just above them — a name reaches some thirty DIPs under its picture, which is why the band given up is the dots'
/// own foot less that room, not merely the dots' height (see `crate::launchpad::Launchpad::dots_line`).
pub fn holds(tall: f32, dots: f32) -> f32 {
    tall.min(dots + FOOT - 40.0)
}

/// The pages' dots, `count` of them, the `page`th lit: a row centred on the screen at height `at` — its own feet,
/// or lifted above the dock where that stands at the foot (see `crate::launchpad::Launchpad::dots_line`).
pub fn dots(frame: &dyn Canvas, count: usize, page: usize, wide: f32, at: f32, dark: bool) {
    if count < 2 {
        return;
    }
    let colours = sheet(dark);
    let from = wide / 2.0 - (count - 1) as f32 * STEP / 2.0;
    for n in 0..count {
        let colour = if n == page { colours.lit } else { colours.dot };
        frame.fill_circle(
            colour,
            Point {
                x: from + n as f32 * STEP,
                y: at,
            },
            if n == page { DOT } else { DOT * 0.82 },
        );
    }
}

/// Which of the `count` dots the point is on, where it is near enough to one. (The nearest, and only within half the
/// room between two: a row of dots is not a slider to be dragged about.)
pub fn dot_at(count: usize, wide: f32, at: f32, x: f32, y: f32) -> Option<usize> {
    if count < 2 || (y - at).abs() > 14.0 {
        return None;
    }
    let from = wide / 2.0 - (count - 1) as f32 * STEP / 2.0;
    let n = ((x - from) / STEP).round();
    let near = (x - (from + n * STEP)).abs() <= STEP / 2.0 + 2.0;
    (n >= 0.0 && n < count as f32 && near).then(|| n as usize)
}

/// Where the search box is: centred at the screen's head, as wide as the screen allows.
pub fn field(wide: f32) -> (f32, f32, f32, f32) {
    let box_wide = (wide * 0.56).min(520.0);
    ((wide - box_wide) / 2.0, 52.0, box_wide, 42.0)
}

/// Where a folder's name stands at the head of its own page: centred in the place the search's box has, and the whole
/// of it the plate the name is written in — as roomy as a name is likely to be, yet narrower than the box, so the
/// head's own edges are not part of it and a click there is nothing at all.
pub fn title(wide: f32) -> (f32, f32, f32, f32) {
    let box_wide = (wide * TITLE_WIDE.0).min(TITLE_WIDE.1);
    ((wide - box_wide) / 2.0, 48.0, box_wide, 50.0)
}

/// Whether the point is on a folder's name, to be written there and then.
pub fn in_title(wide: f32, x: f32, y: f32) -> bool {
    let (from, top, box_wide, box_tall) = title(wide);
    x >= from && x <= from + box_wide && y >= top && y <= top + box_tall
}

/// Whether the point is in the search box, where the keys are written.
pub fn in_field(wide: f32, x: f32, y: f32) -> bool {
    let (from, top, box_wide, box_tall) = field(wide);
    x >= from && x <= from + box_wide && y >= top && y <= top + box_tall
}

/// Where the way back out of a folder is: at the search box's left, where the box's mark stands.
pub fn back(wide: f32) -> (f32, f32, f32, f32) {
    let (from, top, _, box_tall) = field(wide);
    (from - 58.0, top, box_tall, box_tall)
}

/// Where a tile carried to the screen's foot is let go to be fixed to the dock: over the dock's glass where it shows
/// (`glass`, in DIPs from the screen's corner), and along the foot where it does not — the gesture has to have a place
/// to come back to whether the dock is tucked away or on another screen.
pub fn drop(wide: f32, tall: f32, glass: Option<(f32, f32, f32, f32)>) -> (f32, f32, f32, f32) {
    match glass {
        Some(box_) => box_,
        None => {
            let (box_wide, box_tall) = ((wide * 0.42).clamp(220.0, 420.0), 54.0);
            (
                (wide - box_wide) / 2.0,
                tall - box_tall - 12.0,
                box_wide,
                box_tall,
            )
        }
    }
}

/// Whether the point is on the strip, with a little room about it: a drop is not judged to the pixel.
pub fn in_drop(box_: (f32, f32, f32, f32), x: f32, y: f32) -> bool {
    let (from, top, box_wide, box_tall) = box_;
    x >= from - 12.0 && x <= from + box_wide + 12.0 && y >= top - 12.0 && y <= top + box_tall + 12.0
}

/// The strip a carried tile is let go on to fix it to the dock: a plate the dock's glass stands under, its mark and
/// its words. `hot` where the pointer is over it, and `fade` for how far it has come in.
pub fn drop_plate(
    frame: &dyn Canvas,
    box_: (f32, f32, f32, f32),
    hot: bool,
    dark: bool,
    words: &str,
    fade: f32,
) {
    let colours = sheet(dark);
    let (x, y, box_wide, box_tall) = box_;
    let middle = y + box_tall / 2.0;
    let plate = if hot {
        colours.lit.alpha(0.3 * fade)
    } else {
        colours.plate.alpha(0.75 * fade)
    };
    frame.fill_rounded(plate, x, y, box_wide, box_tall, box_tall / 3.0);
    frame.stroke_rounded(
        Fill::Down {
            top: y,
            from: colours.faint.alpha(0.5 * fade),
            bottom: y + box_tall,
            to: colours.faint.alpha(0.2 * fade),
        },
        x,
        y,
        box_wide,
        box_tall,
        box_tall / 3.0,
        1.0,
    );
    frame.icon(
        Icon::Pin,
        Point {
            x: x + box_tall / 2.0 + 6.0,
            y: middle,
        },
        17.0,
        colours.text.alpha(fade),
    );
    centred(
        frame,
        words,
        Font::new(Family::Segoe, 13.5, 500.0),
        colours.text.alpha(fade),
        x + box_wide / 2.0 + 10.0,
        middle - 9.0,
        box_wide - box_tall - 24.0,
    );
}

/// Whether the point is on the way back.
pub fn in_back(wide: f32, x: f32, y: f32) -> bool {
    let (from, top, box_wide, box_tall) = back(wide);
    x >= from && x <= from + box_wide && y >= top && y <= top + box_tall
}

/// The search: its box, its mark, what is written in it and what the input method is composing, or, from nothing yet,
/// what it asks for; the caret where its words end, while it blinks. Standing in a folder there is nothing to search
/// for: `folder` is the name at the head of its page — the name the folder has, or what has been typed in the name's
/// own words while it is being written again — and whether it is being written there and then, drawn as a title in
/// the same place with the way back standing beside it.
pub fn search(
    frame: &dyn Canvas,
    wide: f32,
    words: &str,
    composition: &str,
    caret: bool,
    dark: bool,
    placeholder: &str,
    px: f32,
    folder: Option<(&str, bool)>,
) {
    let colours = sheet(dark);
    let (x, y, box_wide, box_tall) = field(wide);
    let middle = y + box_tall / 2.0;
    let font = Font::new(Family::Segoe, 14.0, 400.0);
    if let Some((written, editing)) = folder {
        let letters = Font::new(Family::Segoe, TITLE, TITLE_HEFT);
        let (_, top, line_wide, line_tall) = title(wide);
        let at = middle - TITLE * 0.72;
        // (The letters have the band less its own inset: the plate written into is the band whole, so it keeps the
        // same room from one letter to the next rather than swelling and shrinking as the name is written.)
        let keeps = (line_wide - 2.0 * TITLE_INSET).max(TITLE * 4.0);
        let head = frame.measure(written, letters).min(keeps);
        if editing {
            frame.fill_rounded(
                colours.plate.alpha(0.85),
                wide / 2.0 - line_wide / 2.0,
                top,
                line_wide,
                line_tall,
                line_tall / 2.0,
            );
        }
        centred(frame, written, letters, colours.text, wide / 2.0, at, keeps);
        if editing && caret {
            frame.fill(
                colours.text,
                wide / 2.0 + head / 2.0 + 3.0,
                at - TITLE * 0.08,
                1.5,
                TITLE * 1.3,
            );
        }
        let (back_from, _, back_tall, _) = back(wide);
        frame.icon(
            Icon::ChevronLeft,
            Point {
                x: back_from + back_tall / 2.0,
                y: middle,
            },
            17.0,
            colours.text,
        );
        return;
    }
    frame.fill_rounded(colours.plate, x, y, box_wide, box_tall, box_tall / 2.0);
    frame.stroke_rounded(
        Fill::Down {
            top: y,
            from: colours.faint.alpha(0.45),
            bottom: y + box_tall,
            to: colours.faint.alpha(0.15),
        },
        x,
        y,
        box_wide,
        box_tall,
        box_tall / 2.0,
        1.0 / px,
    );
    frame.icon(
        Icon::Search,
        Point {
            x: x + 23.0,
            y: middle,
        },
        15.0,
        colours.faint,
    );
    let (from, to) = (x + 46.0, x + box_wide - 16.0);
    let written = format!("{words}{composition}");
    if written.is_empty() {
        frame.text(
            placeholder,
            font,
            colours.faint,
            from,
            middle - 10.0,
            to - from,
            Align::Start,
        );
    } else {
        frame.text(
            &written,
            font,
            colours.text,
            from,
            middle - 10.0,
            to - from,
            Align::Start,
        );
    }
    if caret {
        let head = frame.measure(&written, font).min(to - from);
        frame.fill(colours.text, from + head + 1.0, middle - 11.0, 1.5, 22.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(name: &str) -> Tile {
        Tile {
            name: name.into(),
            target: name.into(),
            picture: None,
            kept: false,
            group: Vec::new(),
        }
    }

    fn group(name: &str, of: usize) -> Tile {
        Tile {
            name: name.into(),
            target: String::new(),
            picture: None,
            kept: false,
            group: (0..of).map(|at| tile(&format!("one {at}"))).collect(),
        }
    }

    #[test]
    fn few_tiles_get_the_biggest() {
        // (A handful of programs on a whole screen: the grid that holds them with the largest tiles.)
        let grid = Grid::of(1920.0, 1080.0, 6);
        assert!(grid.per >= 6);
        assert!(
            grid.side > SIDE.0,
            "a screenful of six is drawn large, not tiny: {}",
            grid.side
        );
        assert_eq!(grid.pages(6), 1);
    }

    #[test]
    fn many_tiles_page() {
        let grid = Grid::of(1920.0, 1080.0, 400);
        assert!(
            grid.per >= ACROSS.1 * DOWN.1,
            "the fullest grid it may: {} × {}",
            grid.cols,
            grid.rows
        );
        assert!(grid.pages(400) > 1);
        // (None of them on a page more than the grid holds.)
        assert!(grid.pages(400) * grid.per >= 400);
    }

    #[test]
    fn the_air_between_tiles_is_the_tiles_own() {
        // (A Mac's launchpad: a tile stands in a cell about twice as wide as itself, rather than shoulder to shoulder
        // with its neighbours.)
        let grid = Grid::of(1440.0, 900.0, 7);
        assert!(
            grid.pitch.0 >= grid.side * 1.8,
            "the columns stand apart: {} against {}",
            grid.pitch.0,
            grid.side
        );
        assert!(
            grid.pitch.1 > grid.side,
            "and there is room under a tile for its name: {}",
            grid.pitch.1
        );
    }

    #[test]
    fn cells_and_hits_agree() {
        let grid = Grid::of(1600.0, 900.0, 30);
        let slack = (grid.pitch.0 - grid.side) / 2.0;
        for index in 0..grid.per.min(30) {
            let (x, y, side, _) = grid.cell(index);
            // The tile's own middle, the room beside it, and the name under it, all answer to it.
            assert_eq!(grid.hit(30, x + side / 2.0, y + side / 2.0), Some(index));
            assert_eq!(grid.hit(30, x - slack + 2.0, y + side / 2.0), Some(index));
            assert_eq!(grid.hit(30, x + side / 2.0, y + side + 6.0), Some(index));
        }
        // Off the grid's left and above it: nothing.
        assert_eq!(grid.hit(30, grid.from.0 - 100.0, grid.from.1), None);
        assert_eq!(grid.hit(30, grid.from.0, grid.from.1 - 40.0), None);
        // Past the last tile of a short page: nothing.
        assert_eq!(
            grid.hit(1, grid.from.0 + grid.pitch.0 * 2.0, grid.from.1),
            None
        );
    }

    #[test]
    fn laid_from_its_start() {
        let (wide, tall) = (1280.0, 800.0);
        let grid = Grid::of(wide, tall, 20);
        let across = (grid.cols as f32 - 1.0) * grid.pitch.0 + grid.side;
        let down = (grid.rows as f32 - 1.0) * grid.pitch.1 + grid.side;
        // (The first tile stands at the room's own corner — its margin, and below the search — rather than being set
        // down out of its middle: what the rows and columns leave over is left at the end of the room.)
        assert_eq!(grid.from, (MARGIN, HEAD));
        // (The last of them still short of the room's two ends, and never off the screen.)
        assert!(
            grid.from.0 + across <= wide - MARGIN + 1.0,
            "within its sides: {across}"
        );
        assert!(
            grid.from.1 + down <= tall - FOOT + 1.0,
            "above its foot dots: {down}"
        );
    }

    #[test]
    fn a_room_tells_both_the_size_and_the_air() {
        // (The room's corners come first, and the rows and the columns have the say after: so many cells across and
        // down as fill it from end to end, and a tile as big as its cell allows — no direction left over to be
        // shared out of a middle.)
        let (wide, tall) = (1387.0, 925.0);
        let (room, cols, rows) = (room(wide, tall), 10, 6);
        let grid = Grid::asked(wide, tall, 60, cols, rows);
        assert!(
            (grid.pitch.0 * cols as f32 - room.0).abs() < 0.5,
            "{}",
            grid.pitch.0
        );
        assert!(
            (grid.pitch.1 * rows as f32 - room.1).abs() < 0.5,
            "{}",
            grid.pitch.1
        );
        // (The size is solved of both ways about at once: the least a cell may be of its picture across and down, its
        // name's room taken off the cell down, and never the most a tile may be.)
        let may = (grid.pitch.0 / AIR.0)
            .min((grid.pitch.1 - NAME) / AIR.1)
            .min(SIDE.1);
        assert!((grid.side - may).abs() < 0.01, "{} vs {}", grid.side, may);
        // (Which leaves the air between the tiles to be what is over — the least across and down, name's room and
        // all.)
        assert!(grid.pitch.0 - grid.side >= (AIR.0 - 1.0) * grid.side - 0.01);
        assert!(grid.pitch.1 - grid.side >= NAME + (AIR.1 - 1.0) * grid.side - 0.01);
        // (The first picture stands in its cell's middle rather than at its corner, and the last short of the room's
        // own end.)
        let (x, _, side, _) = grid.cell(0);
        assert!(x > MARGIN && x < MARGIN + grid.pitch.0 - side, "{x}");
        let (last, _, _, _) = grid.cell(cols * rows - 1);
        assert!(last + side <= wide - MARGIN + 0.01, "{last}");
    }

    #[test]
    fn a_letter_of_what_it_is_called() {
        assert_eq!(tile("Photoshop").letter(), 'P');
        assert_eq!(tile("  wechat ").letter(), 'W');
        assert_eq!(tile("微信").letter(), '微');
        assert_eq!(tile("").letter(), '?');
        assert!(group("工具", 4).is_group());
        assert!(!tile("记事本").is_group());
    }

    #[test]
    fn the_field_holds_its_own_room() {
        let wide = 1440.0;
        let (from, top, box_wide, box_tall) = field(wide);
        assert!(in_field(wide, from + 8.0, top + box_tall / 2.0));
        assert!(in_field(wide, from + box_wide - 8.0, top + box_tall / 2.0));
        // (Out of it on either side, and above and below: the tiles are the tiles, and the head is the head.)
        assert!(!in_field(wide, from - 8.0, top + box_tall / 2.0));
        assert!(!in_field(wide, from + box_wide + 8.0, top + box_tall / 2.0));
        assert!(!in_field(wide, from + box_wide / 2.0, top + box_tall + 8.0));
        assert!(!in_field(wide, from + box_wide / 2.0, top - 8.0));
        // (The way back stands beside it, clear of the tiles' first row.)
        let (left, above, box_wide, box_tall) = back(wide);
        assert!(in_back(wide, left + box_wide / 2.0, above + box_tall / 2.0));
        assert!(!in_back(wide, left - 8.0, above + box_tall / 2.0));
        assert!(!in_back(
            wide,
            left + box_wide + 8.0,
            above + box_tall / 2.0
        ));
        assert!(
            above + box_tall < HEAD,
            "and the head keeps it: {above} + {box_tall} against {HEAD}"
        );
    }

    #[test]
    fn a_carried_tile_has_the_foot_to_be_let_go_at() {
        // (The strip a tile is dropped on to be fixed to the dock: over the dock's glass where it shows, and along the
        // foot where it does not — on the screen either way, and the point answers to it with room about the box.)
        let (wide, tall) = (1920.0, 1080.0);
        let glass = (700.0, 990.0, 520.0, 62.0);
        assert_eq!(drop(wide, tall, Some(glass)), glass);
        assert!(in_drop(glass, 960.0, 1021.0));
        let foot = drop(wide, tall, None);
        assert!(
            foot.1 + foot.3 <= tall,
            "the strip lies along the foot, on the screen: {}",
            foot.1 + foot.3
        );
        assert!(in_drop(foot, wide / 2.0, foot.1 + foot.3 / 2.0));
        assert!(!in_drop(foot, wide / 2.0, foot.1 - 200.0));
    }

    #[test]
    fn dots_answer_where_they_are() {
        let (wide, tall) = (1000.0, 700.0);
        let at = feet(tall);
        assert_eq!(
            dot_at(1, wide, at, wide / 2.0, at),
            None,
            "one page is not a row of dots"
        );
        assert_eq!(dot_at(3, wide, at, wide / 2.0 - STEP, at), Some(0));
        assert_eq!(dot_at(3, wide, at, wide / 2.0, at), Some(1));
        assert_eq!(dot_at(3, wide, at, wide / 2.0 + STEP, at), Some(2));
        assert_eq!(dot_at(3, wide, at, wide / 2.0 + STEP, at + 40.0), None);
        // (The nearest of them, not the first within reach: its middle dot answers to the middle.)
        assert_eq!(
            dot_at(3, wide, at, wide / 2.0 - STEP / 2.0 + 3.0, at),
            Some(1)
        );
        assert_eq!(dot_at(4, wide, at, wide / 2.0 + STEP * 1.6, at), Some(3));
        // (Lifted above the dock, they answer at the lifted height and not at the screen's own foot.)
        assert_eq!(dot_at(3, wide, at - 80.0, wide / 2.0, at - 80.0), Some(1));
        assert_eq!(dot_at(3, wide, at - 80.0, wide / 2.0, at), None);
    }

    #[test]
    fn the_grid_gives_room_to_the_dots() {
        let tall = 1387.0;
        let foot = feet(tall);
        // (Dots at their own feet: the grid gives up a thin band only — the room its names need over the dots, and
        // nothing more.)
        let kept = holds(tall, foot);
        assert!(
            tall - kept <= 16.0 && kept <= tall,
            "the default foot asks the grid for a name's room: {kept} vs {tall}"
        );
        // (Lifted over the dock's glass: the grid ends above them, names and all.)
        let lifted = foot - 120.0;
        assert!(
            holds(tall, lifted) < tall,
            "a lifted row takes room from the grid"
        );
        // (The user's own case: a dense page asked for, 10 across and 8 down, over lifted dots.)
        let grid = Grid::asked(2080.0, holds(tall, lifted), 133, 10, 8);
        assert_eq!((grid.cols, grid.rows), (10, 8));
        let last_row_bottom = grid.from.1 + (grid.rows as f32 - 1.0) * grid.pitch.1 + grid.side;
        // (A name reaches some 30 DIPs under its picture: the whole of that must stand clear of the dots.)
        assert!(
            last_row_bottom + 34.0 <= lifted,
            "the tiles' last row, named, stands clear of the dots: {last_row_bottom} vs {lifted}"
        );
    }

    #[test]
    fn an_asked_grid_scales_its_tiles() {
        let (wide, tall) = (1280.0, 800.0);
        // (No asking: the screen chooses as it always did.)
        assert_eq!(
            Grid::asked(wide, tall, 133, 0, 0),
            Grid::of(wide, tall, 133)
        );
        let few = Grid::asked(wide, tall, 133, 4, 3);
        let many = Grid::asked(wide, tall, 133, 8, 6);
        assert_eq!((few.cols, few.rows, few.per), (4, 3, 12));
        assert_eq!((many.cols, many.rows, many.per), (8, 6, 48));
        // (More tiles to a page: each of them drawn smaller — the scale follows the grid.)
        assert!(many.side < few.side, "{} vs {}", many.side, few.side);
        // (A page's tiles fill it: the asked grid keeps its pages for itself.)
        assert_eq!(many.pages(96), 2);
        // (A big screen asked for a dense page: denser than the old bounds, and the tiles do shrink with it —
        // until they come past what can be read, where the screen's own choice takes over again.)
        let (big_wide, big_tall) = (2080.0, 1387.0);
        let dense = Grid::asked(big_wide, big_tall, 133, 12, 10);
        assert_eq!((dense.cols, dense.rows, dense.per), (12, 10, 120));
        let roomy = Grid::asked(big_wide, big_tall, 133, 6, 4);
        assert!(dense.side < roomy.side, "{} vs {}", dense.side, roomy.side);
        assert!(
            dense.side >= ASKED && dense.side <= SIDE.1,
            "{}",
            dense.side
        );
        // (A 4K-wide screen asked for a page too dense to read: back to the screen's own choice.)
        assert_eq!(
            Grid::asked(800.0, 600.0, 133, 12, 10),
            Grid::of(800.0, 600.0, 133)
        );
    }

    #[test]
    fn a_folder_lies_by_itself() {
        let (wide, tall) = (1280.0, 800.0);
        let (wide_room, tall_room) = room(wide, tall);
        let page = Grid::asked(wide, tall, 200, 10, 6);
        let few = Grid::inside(wide, tall, 4, page.side);
        // (Four of them: four across and one row, at the side the screen's own page gives its tiles — so stepping in
        // or out of a folder asks the pictures for no other size than they were fetched at.)
        assert_eq!((few.cols, few.rows, few.per), (4, 1, 4));
        assert_eq!(few.side, page.side);
        // (The block stood in the room's middle, not at its corner: what the cells leave over is shared before and
        // behind them.)
        let over = (wide_room - 4.0 * few.pitch.0) / 2.0;
        assert!((few.from.0 - MARGIN - over).abs() < 0.01, "{}", few.from.0);
        assert!(over > 1.0, "{}", over);
        // (A folder past the rows the room takes: the same tiles, the same size, and the pages to hold the rest.)
        let many = Grid::inside(wide, tall, 100, page.side);
        assert_eq!(many.cols, IN_ACROSS);
        assert_eq!(many.side, page.side);
        assert!(many.pages(100) > 1);
        let (down, bottom) = (many.from.1, many.rows as f32 * many.pitch.1);
        assert!(
            down + bottom <= tall - FOOT + 0.5,
            "{} + {} vs {}",
            down,
            bottom,
            tall - FOOT
        );
        assert!(
            bottom <= tall_room + 0.5,
            "the rows are shrunk to the room: {bottom} vs {tall_room}"
        );
        // (A crowded folder on a small screen: never bigger tiles than the page it stands in for, whatever it holds.)
        let tight = Grid::inside(800.0, 600.0, 200, Grid::of(800.0, 600.0, 200).side);
        assert!(tight.side <= Grid::of(800.0, 600.0, 200).side + 0.01, "{}", tight.side);
        // (The tiles fill the room's width before they are shrunk: a folder of eight is eight across.)
        let eight = Grid::inside(wide, tall, 8, page.side);
        assert_eq!((eight.cols, eight.rows), (8, 1));
        // (Cells and hits agree, as they do on the screen's own page.)
        for index in 0..many.per {
            let (x, y, side, _) = many.cell(index);
            assert_eq!(
                many.hit(many.per, x + side / 2.0, y + side / 2.0),
                Some(index),
                "the {index}th"
            );
        }
        // (A folder of one is a tile in the middle of the screen, not a tile at its corner.)
        let one = Grid::inside(wide, tall, 1, page.side);
        assert_eq!((one.cols, one.rows), (1, 1));
        assert!(one.from.0 > MARGIN + 100.0, "{}", one.from.0);
    }

    #[test]
    fn a_folder_shows_nine() {
        let (tile, places) = nine(84.0);
        assert_eq!(places.len(), 9);
        // (Nine in three rows, as far apart across as down, and square in the plate.)
        assert!((places[1].0 - places[0].0 - (places[2].0 - places[1].0)).abs() < 0.01);
        assert!((places[3].1 - places[0].1 - (places[1].0 - places[0].0)).abs() < 0.01);
        let (last, top) = (places[8], places[0]);
        assert!((last.0 + tile - (84.0 - top.0)).abs() < 0.01, "{:?}", places);
        assert!(
            (last.1 + tile - (84.0 - top.1)).abs() < 0.01,
            "{:?}",
            places
        );
        // (Read as a picture and not as a smudge: a quarter to a third of the tile's own side.)
        assert!(tile > 84.0 * 0.15 && tile < 84.0 * 0.33, "{}", tile);
        // (The same nine of a bigger folder's tile: bigger plate, bigger pictures, the same proportions.)
        let (wide_tile, _) = nine(116.0);
        assert!((wide_tile / 116.0 - tile / 84.0).abs() < 0.001, "{}", wide_tile);
    }

    #[test]
    fn a_name_is_clicked_where_it_stands() {
        let wide = 1280.0;
        let (from, top, box_wide, box_tall) = title(wide);
        // (The name's band is no wider than the search's box and stands in its middle: the head's own edges are not
        // part of it, so a click high on the page is not a click meant to write a name.)
        assert!(box_wide < field(wide).2, "{box_wide}");
        assert!((from + box_wide / 2.0 - wide / 2.0).abs() < 0.01, "{from}");
        assert!(in_title(wide, wide / 2.0, top + box_tall / 2.0));
        assert!(!in_title(wide, from - 2.0, top + box_tall / 2.0));
        assert!(!in_title(wide, wide / 2.0, top + box_tall + 20.0));
        // (The way back out stands clear of it: a click there goes out rather than beginning to write.)
        let (back_from, back_top, back_wide, back_tall) = back(wide);
        assert!(!in_title(
            wide,
            back_from + back_wide / 2.0,
            back_top + back_tall / 2.0
        ));
        // (The search's box is wider than the name: out on the whole screen a click in the box takes the caret, while
        // standing in a folder a click at the box's own edge is nothing at all.)
        let (field_from, field_top, _, _) = field(wide);
        assert!(!in_title(wide, field_from + 4.0, field_top + 20.0));
        // (The band whole is the plate written into, and keeps its room from one letter to the next: what a name is
        // written in here is the band less its inset, and no band however narrow is less than a word's worth.)
        assert!(box_wide - 2.0 * TITLE_INSET >= 400.0, "{box_wide}");
        for screen in [400.0, 600.0, 800.0, 1024.0, 1280.0, 1920.0, 2560.0] {
            let band = title(screen).2;
            assert!((band - 2.0 * TITLE_INSET).max(TITLE * 4.0) >= TITLE * 4.0);
            assert!(band <= field(screen).2, "{screen}: {band} over {}", field(screen).2);
        }
    }
}
