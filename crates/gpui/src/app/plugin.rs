// Directly inspired by Bevy's plugin.rs: https://github.com/bevyengine/bevy/blob/c84564a8dc3f9693b50a7c88e043e80cb7c08f2b/crates/bevy_app/src/plugin.rs

use std::any::Any;

use crate::App;

/// A plugin for the gpui application.
///
/// Plugins in GPUI are first-class, meaning that they all have
/// equal access to contribute state and behavior to the application.
///
/// GPUI apps revolve around the `App`, and having a `cx: &mut App` in
/// hand allows one to interact with virtually the entire application.
///
/// ```rust
/// use zed_tbd::*;
/// use gpui::*;
///
/// pub fn main() {
///     Application::new()
///         .add_plugin(ZedPlugins)
///         .add_plugin(GlobalClickerPlugin)
///         .run();
/// }
///
/// pub struct GlobalClicker(u32);
/// impl Global for GlobalClicker {}
///
/// pub struct GlobalClickerPlugin;
/// impl Plugin for GlobalClickerPlugin {
///     fn build(&self, cx: &mut App) {
///         cx.set_global(GlobalClicker(0));
///         assert_eq!(cx.global::<GlobalClicker>().unwrap().0, 0);
///         // TODO implement cool example with mouse events or something
///     }
/// }
/// ```
pub trait Plugin: Any + Send + Sync {
    /// Builds this plugin into the GPUI app
    fn build(&self, cx: &mut App);
}

impl<F> Plugin for F
where
    F: 'static + Send + Sync + Fn(&mut App),
{
    fn build(&self, cx: &mut App) {
        (self)(cx);
    }
}
