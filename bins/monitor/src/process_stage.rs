//! [`ProcessStage`] -- the process tiles in motion.
//!
//! Aims [`TileGrowth`] at the field's arrangement and paints every process
//! and "more" tile at the shape it has this frame, including the ones
//! shrinking away.

use platform::draw::{Canvas, Rect};

use crate::damage::DamageList;
use crate::palette::MonitorPalette;
use crate::process_tiles::{MoreFace, ProcessFace, ProcessTiles};
use crate::tile_field::TileField;
use crate::tile_growth::{Destination, Shape, TileGrowth};
use crate::tile_hover::TileHover;
use crate::tile_key::TileKey;
use crate::tile_layout::TileKind;
use crate::tile_metrics::TileMetrics;
use crate::typography::Typography;

/// What one frame of the stage paints with.
pub struct StageFrame<'a> {
    pub palette: &'a MonitorPalette,
    pub metrics: TileMetrics,
    pub field: &'a TileField,
    pub hover: &'a TileHover,
    pub full: bool,
    pub stale: &'a [TileKind],
}

#[derive(Default)]
pub struct ProcessStage {
    growth: TileGrowth,
    was_moving: bool,
}

impl ProcessStage {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Follow the field's arrangement, laid out in `bounds`, and move every
    /// tile on to `now_ms`.
    pub fn step(&mut self, field: &TileField, bounds: Rect, now_ms: u64) {
        let targets: Vec<Destination> = field
            .arrangement()
            .tiles
            .iter()
            .filter_map(|tile| {
                Some(Destination {
                    key: TileKey::of(&tile.kind)?,
                    rect: tile.rect,
                })
            })
            .collect();
        self.growth
            .aim(field.view().group(), bounds, &targets, now_ms);
        self.growth.advance(now_ms);
    }

    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.growth.is_animating()
    }

    /// Paint the process tiles. While anything moves, and on the frame it
    /// lands, the whole process region repaints; otherwise only a full
    /// repaint or a `stale` tile draws.
    pub fn paint(
        &mut self,
        canvas: &mut Canvas<'_>,
        typography: &mut Typography,
        frame: &StageFrame<'_>,
    ) -> DamageList {
        let moving = self.growth.is_animating() || self.was_moving;
        self.was_moving = self.growth.is_animating();
        let mut damage = DamageList::new();
        let region = moving.then(|| self.region(frame.field));
        if let Some(region) = region.filter(|_| !frame.full) {
            canvas.fill_rect_glass(region, frame.palette.background);
            damage.push(region);
        }
        let painter = ProcessTiles::new(frame.palette, frame.metrics);
        for tile in &frame.field.arrangement().tiles {
            let Some(key) = TileKey::of(&tile.kind) else {
                continue;
            };
            let stale = frame
                .stale
                .iter()
                .any(|kind| TileKey::of(kind).as_ref() == Some(&key));
            if !(moving || frame.full || stale) {
                continue;
            }
            if stale && !moving && !frame.full {
                canvas.fill_rect_glass(tile.rect, frame.palette.background);
                damage.push(tile.rect);
            }
            let shape = self
                .growth
                .shape(&key)
                .unwrap_or_else(|| Shape::resting(tile.rect));
            Self::paint_one(&painter, canvas, typography, frame, &tile.kind, shape);
        }
        if moving {
            for (key, shape) in self.growth.leaving() {
                let kind = match key {
                    TileKey::Process(name) => TileKind::Process(name),
                    TileKey::More => TileKind::More { count: 0 },
                };
                Self::paint_one(&painter, canvas, typography, frame, &kind, shape);
            }
        }
        damage
    }

    fn region(&self, field: &TileField) -> Rect {
        field
            .arrangement()
            .tiles
            .iter()
            .filter(|tile| TileKey::of(&tile.kind).is_some())
            .map(|tile| tile.rect)
            .chain(
                self.growth
                    .leaving()
                    .into_iter()
                    .map(|(_, shape)| shape.rect),
            )
            .reduce(Self::union)
            .unwrap_or(Rect::new(0, 0, 0, 0))
    }

    fn union(a: Rect, b: Rect) -> Rect {
        let x = a.x.min(b.x);
        let y = a.y.min(b.y);
        Rect::new(
            x,
            y,
            a.right().max(b.right()) - x,
            a.bottom().max(b.bottom()) - y,
        )
    }

    fn paint_one(
        painter: &ProcessTiles<'_>,
        canvas: &mut Canvas<'_>,
        typography: &mut Typography,
        frame: &StageFrame<'_>,
        kind: &TileKind,
        shape: Shape,
    ) {
        let field = frame.field;
        let hovered = frame.hover.is(kind);
        match kind {
            TileKind::Process(name) => {
                let Some(group) = field.group(name) else {
                    return;
                };
                let face = ProcessFace {
                    name,
                    memory_kb: group.memory_kb,
                    cpu: field.cpu(name),
                    count: group.count,
                    heat: field.heat(name),
                    hovered,
                    grown: shape.grown,
                };
                painter.paint(canvas, typography, shape.rect, &face);
            }
            TileKind::More { .. } => {
                let tail = field.tail();
                let face = MoreFace {
                    groups: tail.groups,
                    memory_kb: tail.memory_kb,
                    hovered,
                    grown: shape.grown,
                };
                painter.paint_more(canvas, typography, shape.rect, &face);
            }
            TileKind::Info | TileKind::Chart(_) => {}
        }
    }
}
