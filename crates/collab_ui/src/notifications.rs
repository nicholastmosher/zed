mod collab_notification;
pub mod incoming_call_notification;
pub mod project_shared_notification;

#[cfg(feature = "stories")]
mod stories;

use gpui::App;

#[cfg(feature = "stories")]
pub use stories::*;

pub fn init(cx: &mut App) {
    incoming_call_notification::init(cx);
    project_shared_notification::init(cx);
}
