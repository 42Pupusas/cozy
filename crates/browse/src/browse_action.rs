use crate::edit_action::EditAction;
use widget::ListCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseAction {
    List(ListCommand),
    Insert(char),
    Edit(EditAction),
    BeginFilter,
    EndFilter,
    CancelFilter,
    GoUp,
    Refresh,
    ToggleHidden,
}
