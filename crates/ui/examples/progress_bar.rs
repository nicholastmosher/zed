use gpui::{AppContext, WindowOptions};
use settings::{Settings, SettingsStore};
use theme::ThemeSettings;
use ui::{App, ProgressBar, Window};

fn main() -> anyhow::Result<()> {
    gpui::Application::new().run(|cx| {
        cx.set_global(SettingsStore::new(cx));
        ThemeSettings::register(cx);
        cx.open_window(WindowOptions::default(), |window, cx| {
            cx.new(|cx| ProgressBar::new("bar", 0.5, 1.0, cx))
        })
        .unwrap();
    });
    Ok(())
}
