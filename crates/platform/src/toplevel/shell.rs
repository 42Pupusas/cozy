//! Standing up the xdg-shell object chain, and answering the shell events
//! that keep a window alive.
//!
//! Separated from the window struct because these are pure protocol
//! transactions with no dependency on buffers, painters, or input: build the
//! `wl_surface` -> `xdg_surface` -> `xdg_toplevel` chain, name the window,
//! ask for decorations, and answer pings. Keeping them here means the window
//! type reads as orchestration rather than as a wall of `MessageBuilder`
//! calls.

use crate::conn::{Sender, WindowSender};
use crate::error::Result;
use crate::id::{
    CompositorId, DecorationManagerId, SurfaceId, ToplevelDecorationId, ToplevelId, WmBaseId,
    XdgSurfaceId,
};
use crate::protocol::{
    CompositorRequest, DecorationManagerRequest, DecorationMode, SurfaceRequest,
    ToplevelDecorationRequest, ToplevelRequest, WmBaseRequest, XdgSurfaceRequest,
};
use crate::wire::MessageBuilder;

/// The three objects that make up a mapped application window.
#[derive(Debug, Clone, Copy)]
pub struct ShellSurface {
    pub surface: SurfaceId,
    pub xdg_surface: XdgSurfaceId,
    pub toplevel: ToplevelId,
    /// Present only when the compositor supports server-side decorations.
    pub decoration: Option<ToplevelDecorationId>,
}

impl ShellSurface {
    /// Create the object chain and commit the initial, **buffer-less** state.
    ///
    /// That final empty commit is not optional and not a mistake: xdg-shell
    /// requires a commit with no buffer attached to trigger the compositor's
    /// first `configure`. Attaching pixels before that configure is acked is
    /// a protocol error that kills the client -- see
    /// [`super::lifecycle::Lifecycle`], which enforces the other half of the
    /// rule.
    pub fn create(
        sender: &mut Sender,
        compositor: CompositorId,
        wm_base: WmBaseId,
        decoration_manager: Option<DecorationManagerId>,
        scale: i32,
        title: &str,
        app_id: &str,
    ) -> Result<Self> {
        let surface: SurfaceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(compositor, CompositorRequest::CreateSurface).new_id(surface),
        )?;
        // Declare that our buffers are already at `scale`x density, so the
        // compositor composites us 1:1 instead of resampling (and blurring)
        // a 1x buffer up to fill the physical pixels.
        sender.request(MessageBuilder::new(surface, SurfaceRequest::SetBufferScale).i32(scale))?;

        let xdg_surface: XdgSurfaceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(wm_base, WmBaseRequest::GetXdgSurface)
                .new_id(xdg_surface)
                .object(surface),
        )?;

        let toplevel: ToplevelId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(xdg_surface, XdgSurfaceRequest::GetToplevel).new_id(toplevel),
        )?;
        sender.request(MessageBuilder::new(toplevel, ToplevelRequest::SetTitle).string(title))?;
        // The title is human-facing and apps rewrite it constantly, so it is
        // a poor key for window rules. `app_id` is stable for the process's
        // lifetime, which is what compositors match on.
        sender.request(MessageBuilder::new(toplevel, ToplevelRequest::SetAppId).string(app_id))?;

        let decoration = Self::request_decorations(sender, toplevel, decoration_manager)?;

        // The buffer-less commit that provokes the first configure.
        sender.request(MessageBuilder::new(surface, SurfaceRequest::Commit))?;

        Ok(Self {
            surface,
            xdg_surface,
            toplevel,
            decoration,
        })
    }

    /// Ask the compositor to draw this window's titlebar and borders.
    ///
    /// We *request* server-side decorations rather than assume them: the
    /// protocol lets the compositor answer with client-side instead, and on
    /// compositors that don't implement the extension at all (GNOME) there
    /// is no decoration object to make. In every one of those cases the
    /// window still works -- it is simply undecorated -- so this never fails
    /// the window's construction.
    fn request_decorations(
        sender: &mut Sender,
        toplevel: ToplevelId,
        manager: Option<DecorationManagerId>,
    ) -> Result<Option<ToplevelDecorationId>> {
        let Some(manager) = manager else {
            return Ok(None);
        };
        let decoration: ToplevelDecorationId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(manager, DecorationManagerRequest::GetToplevelDecoration)
                .new_id(decoration)
                .object(toplevel),
        )?;
        sender.request(
            MessageBuilder::new(decoration, ToplevelDecorationRequest::SetMode)
                .u32(DecorationMode::ServerSide as u32),
        )?;
        Ok(Some(decoration))
    }

    /// Answer an `xdg_wm_base.ping` so the compositor doesn't decide we've
    /// hung and kill the window.
    ///
    /// Takes the `WindowSender` (not `Sender`) because this fires from
    /// inside event dispatch, where the real connection is borrowed by the
    /// event loop.
    pub fn pong(window_tx: &WindowSender, wm_base: WmBaseId, serial: u32) {
        window_tx.request(MessageBuilder::new(wm_base, WmBaseRequest::Pong).u32(serial));
    }

    /// Acknowledge an `xdg_surface.configure`.
    pub fn ack_configure(&self, window_tx: &WindowSender, serial: u32) {
        window_tx.request(
            MessageBuilder::new(self.xdg_surface, XdgSurfaceRequest::AckConfigure).u32(serial),
        );
    }

    /// Set the window title, which apps change at runtime.
    pub fn set_title(&self, window_tx: &WindowSender, title: &str) {
        window_tx
            .request(MessageBuilder::new(self.toplevel, ToplevelRequest::SetTitle).string(title));
    }

    /// Tear the chain down in the reverse order it was built.
    ///
    /// Order matters: destroying the `xdg_surface` while its `xdg_toplevel`
    /// still exists is a protocol error, because the toplevel is a child
    /// role object of the surface.
    pub fn destroy(&self, window_tx: &WindowSender) {
        window_tx.request(MessageBuilder::new(self.toplevel, ToplevelRequest::Destroy));
        window_tx.request(MessageBuilder::new(
            self.xdg_surface,
            XdgSurfaceRequest::Destroy,
        ));
        window_tx.request(MessageBuilder::new(self.surface, SurfaceRequest::Destroy));
    }
}
