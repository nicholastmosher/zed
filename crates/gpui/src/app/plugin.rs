use std::any::Any;

use crate::App;

/// A plugin for the gpui application.
pub trait Plugin: Any + Send + Sync {
    /// Builds this plugin into the GPUI app, pre-launch
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
            app.plugins.push_back(Box::new(self));
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
