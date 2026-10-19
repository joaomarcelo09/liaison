//! System tray icon (freedesktop StatusNotifierItem, shown by waybar, KDE, …). It lives in the
//! hub so it stays in the bar while the window is closed. An orange dot marks unread messages;
//! clicking opens the app, the menu lists the chats waiting for you.

use crate::{notifier, state::SharedHub};
use ksni::{Icon, MenuItem, ToolTip, Tray, TrayMethods, menu::StandardItem};
use liaison_core::{Event, Session};
use resvg::{tiny_skia, usvg};
use std::{sync::LazyLock, time::Duration};
use tokio::sync::broadcast::error::RecvError;

const SVG: &str = include_str!("../../../packaging/liaison.svg");
const SIZES: [u32; 4] = [22, 32, 48, 64];
const DOT: [u8; 3] = [0xff, 0x8a, 0x1f];
/// Most chats listed in the menu.
const MENU_CHATS: usize = 8;

static PLAIN: LazyLock<Vec<Icon>> = LazyLock::new(|| render(false));
static DOTTED: LazyLock<Vec<Icon>> = LazyLock::new(|| render(true));

struct Chat {
    id: String,
    name: String,
    unread: i64,
}

struct LiaisonTray {
    hub: SharedHub,
    /// Chats with unread messages, most recent first.
    chats: Vec<Chat>,
}

impl LiaisonTray {
    fn unread(&self) -> i64 {
        self.chats.iter().map(|c| c.unread).sum()
    }
}

impl Tray for LiaisonTray {
    fn id(&self) -> String {
        "liaison".into()
    }

    fn title(&self) -> String {
        "Liaison".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Communications
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        if self.chats.is_empty() { PLAIN.clone() } else { DOTTED.clone() }
    }

    fn tool_tip(&self) -> ToolTip {
        let description = match self.unread() {
            0 => "No unread messages".to_string(),
            n => {
                let chats: Vec<String> = self.chats.iter().take(3).map(|c| format!("{} ({})", c.name, c.unread)).collect();
                format!("{n} unread · {}", chats.join(", "))
            }
        };
        ToolTip { title: "Liaison".into(), description, ..Default::default() }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let first = self.chats.first().map(|c| c.id.clone());
        notifier::open_app(&self.hub, first.as_deref());
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: "Open Liaison".into(),
                icon_name: "liaison".into(),
                activate: Box::new(|this: &mut Self| notifier::open_app(&this.hub, None)),
                ..Default::default()
            }
            .into(),
        ];
        if !self.chats.is_empty() {
            items.push(MenuItem::Separator);
            for chat in self.chats.iter().take(MENU_CHATS) {
                let id = chat.id.clone();
                items.push(
                    StandardItem {
                        label: format!("{} ({})", chat.name.replace('_', "__"), chat.unread),
                        activate: Box::new(move |this: &mut Self| notifier::open_app(&this.hub, Some(&id))),
                        ..Default::default()
                    }
                    .into(),
                );
            }
            if self.chats.len() > MENU_CHATS {
                items.push(
                    StandardItem {
                        label: format!("… and {} more", self.chats.len() - MENU_CHATS),
                        enabled: false,
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }
        items
    }

    fn watcher_online(&self) {
        tracing::info!("tray: status bar found, icon shown");
    }

    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        tracing::info!("tray: no status bar tray yet ({reason:?}); the icon will appear when one starts");
        true // keep running: the bar may start later or come back (e.g. waybar restarted)
    }
}

/// Starts the tray icon if enabled; it follows the hub's events to keep the dot current.
pub fn start(hub: SharedHub) {
    if !hub.config().desktop.tray {
        return;
    }
    tokio::spawn(async move {
        let tray = LiaisonTray { hub: hub.clone(), chats: unread_chats(&hub) };
        // The hub usually starts at login before the bar (waybar, snixembed on i3, …) is up:
        // keep the icon waiting and register it as soon as a tray appears.
        let handle = match tray.assume_sni_available(true).spawn().await {
            Ok(handle) => handle,
            Err(e) => {
                tracing::warn!("tray icon unavailable: {e}");
                return;
            }
        };
        let mut events = hub.subscribe();
        loop {
            match events.recv().await {
                Ok(Event::Session { .. } | Event::SessionRemoved { .. } | Event::Message { .. }) | Err(RecvError::Lagged(_)) => {}
                Ok(_) => continue,
                Err(RecvError::Closed) => break,
            }
            // Coalesce bursts (a reply often brings several events).
            tokio::time::sleep(Duration::from_millis(250)).await;
            while events.try_recv().is_ok() {}
            let chats = unread_chats(&hub);
            if handle.update(|tray| tray.chats = chats).await.is_none() {
                break;
            }
        }
    });
}

fn unread_chats(hub: &SharedHub) -> Vec<Chat> {
    match hub.db.sessions(false) {
        Ok(sessions) => sessions
            .iter()
            .filter(|s| s.unread > 0)
            .map(|s: &Session| Chat { id: s.id.clone(), name: s.display_name(), unread: s.unread })
            .collect(),
        Err(e) => {
            tracing::warn!("tray: listing sessions failed: {e}");
            Vec::new()
        }
    }
}

/// The app icon at a few sizes, as ARGB32 pixmaps, optionally with the unread dot.
fn render(dot: bool) -> Vec<Icon> {
    let tree = match usvg::Tree::from_str(SVG, &usvg::Options::default()) {
        Ok(tree) => tree,
        Err(e) => {
            tracing::warn!("tray: bad icon: {e}");
            return Vec::new();
        }
    };
    SIZES
        .iter()
        .filter_map(|&size| {
            let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
            let scale = size as f32 / tree.size().width();
            resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
            if dot {
                draw_dot(&mut pixmap);
            }
            let data = pixmap
                .pixels()
                .iter()
                .flat_map(|p| {
                    let c = p.demultiply();
                    [c.alpha(), c.red(), c.green(), c.blue()]
                })
                .collect();
            Some(Icon { width: size as i32, height: size as i32, data })
        })
        .collect()
}

/// An orange dot in the top-right corner, cut out from the icon by a transparent ring.
fn draw_dot(pixmap: &mut tiny_skia::Pixmap) {
    let size = pixmap.width() as f32;
    let r = size * 0.24;
    let (cx, cy) = (size - r, r);
    let ring = size * 0.07;
    let fill = |pixmap: &mut tiny_skia::Pixmap, radius: f32, color: tiny_skia::Color, blend: tiny_skia::BlendMode| {
        if let Some(path) = tiny_skia::PathBuilder::from_circle(cx, cy, radius) {
            let mut paint = tiny_skia::Paint::default();
            paint.set_color(color);
            paint.anti_alias = true;
            paint.blend_mode = blend;
            pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
        }
    };
    fill(pixmap, r + ring, tiny_skia::Color::BLACK, tiny_skia::BlendMode::DestinationOut);
    fill(pixmap, r, tiny_skia::Color::from_rgba8(DOT[0], DOT[1], DOT[2], 0xff), tiny_skia::BlendMode::SourceOver);
}

#[cfg(test)]
mod tests {
    #[test]
    fn icons_render_at_every_size() {
        for (plain, dotted) in super::render(false).iter().zip(super::render(true).iter()) {
            assert_eq!(plain.data.len(), (plain.width * plain.height * 4) as usize);
            assert_ne!(plain.data, dotted.data, "the dot changes the icon");
        }
        assert_eq!(super::render(true).len(), super::SIZES.len());
        if let Ok(dir) = std::env::var("LIAISON_TRAY_PNG_DIR") {
            // Debug aid: dump the 64px icons as PNG.
            for (name, icons) in [("plain", super::render(false)), ("dotted", super::render(true))] {
                let icon = icons.last().unwrap();
                let mut pixmap = resvg::tiny_skia::Pixmap::new(icon.width as u32, icon.height as u32).unwrap();
                for (px, argb) in pixmap.pixels_mut().iter_mut().zip(icon.data.chunks_exact(4)) {
                    *px = resvg::tiny_skia::ColorU8::from_rgba(argb[1], argb[2], argb[3], argb[0]).premultiply();
                }
                pixmap.save_png(format!("{dir}/tray-{name}.png")).unwrap();
            }
        }
    }
}
