use gpui::*;
use serde::Deserialize;

#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct P2pPanelSettings {
    pub default_width: Pixels,
}
