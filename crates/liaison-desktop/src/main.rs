//! liaison-desktop: the Liaison chat window.

mod app;
mod hub;
mod style;
mod view;

use liaison_core::{HubClient, api::HealthResponse};

fn main() -> iced::Result {
    let mut args = std::env::args().skip(1);
    let mut session = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--session" => session = args.next(),
            "--version" | "-V" => {
                println!("liaison-desktop {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ => {}
        }
    }
    if focus_running_instance(session.clone()) {
        return Ok(());
    }

    iced::application(move || app::App::new(session.clone()), app::App::update, view::view)
        .default_font(style::ui_font())
        .title(app::App::title)
        .theme(app::App::theme)
        .scale_factor(app::App::scale_factor)
        .subscription(app::App::subscription)
        .window(iced::window::Settings {
            size: iced::Size::new(1180.0, 780.0),
            min_size: Some(iced::Size::new(760.0, 480.0)),
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: "liaison".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .run()
}

/// Only one window: when the app is already open, bring it to the front instead.
fn focus_running_instance(session: Option<String>) -> bool {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return false };
    rt.block_on(async {
        let client = HubClient::default().with_timeout(std::time::Duration::from_secs(2));
        match client.get::<HealthResponse>("/v1/health").await {
            Ok(health) if health.ui_connected => client
                .post::<_, serde_json::Value>("/v1/ui/focus", &serde_json::json!({ "session_id": session }))
                .await
                .is_ok(),
            _ => false,
        }
    })
}
