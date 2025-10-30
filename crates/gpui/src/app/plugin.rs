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
///         .add_plugins(ZedPlugins)
///         .add_plugins(GlobalClickerPlugin)
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

/// Trait for types that can be inserted together as GPUI plugins
///
/// This includes single plugins and tuples of plugins.
pub trait Plugins<Marker>: sealed::Plugins<Marker> {}

impl<Marker, T> Plugins<Marker> for T where T: sealed::Plugins<Marker> {}

mod sealed {
    use bevy_utils_proc_macros::all_tuples;

    use super::Plugin;
    use crate::App;

    pub struct PluginMarker;
    pub struct PluginsTupleMarker;

    pub trait Plugins<Marker> {
        fn add_to_app(self, app: &mut App);
    }

    impl<P: Plugin> Plugins<PluginMarker> for P {
        #[track_caller]
        fn add_to_app(self, app: &mut App) {
            _ = app.plugins.push_back(Box::new(self));
        }
    }

    macro_rules! impl_plugins_tuples {
        ($(#[$meta:meta])* $(($param: ident, $plugins: ident)),*) => {
            $(#[$meta])*
            impl<$($param, $plugins),*> Plugins<(PluginsTupleMarker, $($param,)*)> for ($($plugins,)*)
            where
                $($plugins: Plugins<$param>),*
            {
                // We use `allow` instead of `expect` here because the lint is not generated for all cases.
                #[allow(non_snake_case, reason = "`all_tuples!()` generates non-snake-case variable names.")]
                #[allow(unused_variables, reason = "`app` is unused when implemented for the unit type `()`.")]
                #[track_caller]
                fn add_to_app(self, app: &mut App) {
                    let ($($plugins,)*) = self;
                    $($plugins.add_to_app(app);)*
                }
            }
        }
    }

    all_tuples!(
        #[doc(fake_variadic)]
        impl_plugins_tuples,
        0,
        15,
        P,
        S
    );
}
