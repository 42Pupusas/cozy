/// A global interface we bind by name, with the maximum version we support.
pub struct Interface {
    pub name: &'static str,
    pub version: u32,
}
