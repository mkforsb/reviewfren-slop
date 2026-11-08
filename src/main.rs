mod app;
mod claude;
mod code_view;
mod debug;
mod diff;
mod fonts;
mod git;
mod highlight;
mod lsp;
mod mason;
mod selectable_text;
mod settings;
mod style;
mod view;

fn main() -> iced::Result {
    debug::install_signal_logging();
    // The UI font can only be set when the application starts.
    let settings = settings::Settings::load();
    fonts::set_ui(&settings.ui_font);
    fonts::set_code(&settings.code_font, settings.code_font_size);
    let result = iced::application(app::App::new, app::App::update, app::App::view)
        .default_font(fonts::ui())
        .scale_factor(app::App::scale_factor)
        .title(app::App::title)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .window_size((1500.0, 950.0))
        .antialiasing(true)
        .run();
    debug::dlog!("iced run() returned: {result:?}");
    result
}
