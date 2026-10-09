//! [`SurfaceTree`] -- the window surface plus child subsurfaces for the
//! solid rects (one per square rect, seven per rounded one), updated from a
//! [`SolidFrame`] with only the requests that changed.

use crate::conn::WindowSender;
use crate::draw::Rect;
use crate::error::Result;
use crate::id::{RegionId, SurfaceId};
use crate::protocol::CompositorRequest;
use crate::wire::MessageBuilder;

use super::logical_rect::LogicalRect;
use super::mask_pool::MaskPool;
use super::rounded_sections::RoundedSections;
use super::scene_globals::SceneGlobals;
use super::section_fill::SectionFill;
use super::section_state::SectionState;
use super::solid_frame::SolidFrame;
use super::solid_pool::SolidPool;
use super::solid_section::{SectionFactory, SolidSection};

/// Requests sent and sections touched by one [`SurfaceTree::apply`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TreeUpdate {
    pub requests: usize,
    pub sections: usize,
}

pub struct SurfaceTree {
    factory: SectionFactory,
    root: SolidSection,
    children: Vec<SolidSection>,
    pool: SolidPool,
    masks: MaskPool,
    states: Vec<SectionState>,
}

impl SurfaceTree {
    /// A child with no rect this frame keeps its surface, hidden.
    const HIDDEN: SectionState = SectionState {
        rect: LogicalRect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        },
        fill: SectionFill::Solid(0),
    };

    pub fn new(
        window_tx: &mut WindowSender,
        globals: &SceneGlobals,
        surface: SurfaceId,
    ) -> Result<Self> {
        let no_input: RegionId = window_tx.alloc_id();
        window_tx.request(
            MessageBuilder::new(globals.compositor, CompositorRequest::CreateRegion)
                .new_id(no_input),
        );
        let factory = SectionFactory {
            compositor: globals.compositor,
            subcompositor: globals.subcompositor,
            viewporter: globals.viewporter,
            parent: surface,
            no_input,
        };
        Ok(Self {
            factory,
            root: SolidSection::root(window_tx, globals.viewporter, surface),
            children: Vec::new(),
            pool: SolidPool::new(window_tx, globals.shm, globals.single_pixel)?,
            masks: MaskPool::new(window_tx, globals.shm)?,
            states: Vec::new(),
        })
    }

    /// Bring every section level with `frame`. The caller commits the root
    /// surface afterwards, which applies every child's state with it.
    pub fn apply(
        &mut self,
        window_tx: &mut WindowSender,
        frame: &SolidFrame,
        physical: (i32, i32),
        scale: i32,
    ) -> Result<TreeUpdate> {
        let whole = Rect::new(0, 0, physical.0, physical.1);
        let mut update = TreeUpdate::default();
        let background = SectionState {
            rect: LogicalRect::from_physical(whole, scale),
            fill: SectionFill::Solid(SolidFrame::argb(frame.background)),
        };
        let mut pools = (&mut self.pool, &mut self.masks);
        update.add(Self::apply_one(
            &mut pools,
            window_tx,
            &mut self.root,
            background,
        )?);
        Self::expand(frame, scale, &mut self.states);
        while self.children.len() < self.states.len() {
            self.children
                .push(SolidSection::child(window_tx, self.factory));
        }
        for (index, child) in self.children.iter_mut().enumerate() {
            let state = self.states.get(index).copied().unwrap_or(Self::HIDDEN);
            update.add(Self::apply_one(&mut pools, window_tx, child, state)?);
        }
        Ok(update)
    }

    fn expand(frame: &SolidFrame, scale: i32, states: &mut Vec<SectionState>) {
        states.clear();
        for solid in &frame.rects {
            let scale_div = scale.max(1);
            RoundedSections::expand(
                LogicalRect::from_physical(solid.rect, scale),
                SolidFrame::argb(solid.color),
                i32::from(solid.radius) / scale_div,
                scale_div,
                states,
            );
        }
    }

    fn apply_one(
        (pool, masks): &mut (&mut SolidPool, &mut MaskPool),
        window_tx: &mut WindowSender,
        section: &mut SolidSection,
        state: SectionState,
    ) -> Result<usize> {
        let buffer = match (state.rect.is_visible(), state.fill) {
            (false, _) => None,
            (true, SectionFill::Solid(argb)) => Some(pool.buffer_for(window_tx, argb)?),
            (true, SectionFill::Corner { argb, side, .. }) => {
                Some(masks.buffer_for(window_tx, argb, side)?)
            }
        };
        Ok(section.apply(window_tx, state, |_| buffer))
    }

    /// Corner masks the compositor holds, and their shm bytes.
    #[must_use]
    pub fn masks(&self) -> (usize, usize) {
        (self.masks.masks(), self.masks.bytes())
    }

    /// Distinct colour buffers the compositor holds for this window.
    #[must_use]
    pub fn colours(&self) -> usize {
        self.pool.colours()
    }

    #[must_use]
    pub const fn sections(&self) -> usize {
        self.children.len() + 1
    }

    pub fn destroy(&self, window_tx: &WindowSender) {
        for child in self.children.iter().rev() {
            child.destroy(window_tx);
        }
        self.root.destroy(window_tx);
    }
}

impl TreeUpdate {
    const fn add(&mut self, requests: usize) {
        if requests > 0 {
            self.requests += requests;
            self.sections += 1;
        }
    }
}
