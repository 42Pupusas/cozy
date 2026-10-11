use browse::BrowseAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChooserAction {
    Browse(BrowseAction),
    Accept,
    Cancel,
}
