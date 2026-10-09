/// `wl_display` is always object id 1; it's the root of everything.
pub const DISPLAY_ID: u32 = 1;

requests! {
    /// `wl_display` requests.
    DisplayRequest {
        Sync = 0,
        GetRegistry = 1,
    }
}
events! {
    /// `wl_display` events.
    DisplayEvent {
        /// A request referenced an invalid object — fatal protocol error.
        Error = 0,
        /// The server has deleted the object with the given id.
        DeleteId = 1,
    }
}

requests! {
    /// `wl_registry` requests — binds the compositor's global singletons.
    RegistryRequest {
        Bind = 0,
    }
}
events! {
    /// `wl_registry` events.
    RegistryEvent {
        /// A global is available: `name`, `interface`, `version`.
        Global = 0,
        /// A previously announced global went away.
        GlobalRemove = 1,
    }
}

events! {
    /// `wl_callback` events (used for `wl_surface.frame` throttling).
    CallbackEvent {
        /// The callback has fired: `callback_data` (a timestamp we ignore).
        Done = 0,
    }
}
