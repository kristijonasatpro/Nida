//! Toast notification overlay rendering.

use super::super::notifications::notification_color;
use super::super::{NotificationUI, UIActions};

/// Lines of a message a toast shows; the rest is on its hover.
const LINES: usize = 6;

/// How far down the window toasts may stack: five long ones once ran down
/// over the switcher's CUT and AUTO. Those that do not fit show as room is
/// made, unless their time runs out first; every notice is in the log.
const STACK_SHARE: f32 = 0.4;

pub(super) fn render_notifications(
    ctx: &egui::Context,
    notifications: &[NotificationUI],
    actions: &mut UIActions,
) {
    if notifications.is_empty() {
        return;
    }

    let screen_rect = ctx.content_rect();
    let toast_width = 360.0;
    let margin = 12.0;
    let spacing = 6.0;
    // Left of the text, the accent bar; right of it, the dismiss button.
    let text_width = toast_width - 12.0 - 28.0;
    let x = screen_rect.right() - toast_width - margin;
    let bottom = screen_rect.top() + screen_rect.height() * STACK_SHARE;
    let mut y = screen_rect.top() + margin;

    for (i, notif) in notifications.iter().enumerate() {
        // The message wrapped, up to `LINES` lines, the toast as tall as it:
        // cut to one line, most notices lost what happened and what to do
        // about it — which reached only the log.
        let mut job = egui::text::LayoutJob::simple(
            notif.message.clone(),
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(220),
            text_width,
        );
        job.wrap.max_rows = LINES;
        job.wrap.overflow_character = Some('…');
        let text = ctx.fonts_mut(|fonts| fonts.layout_job(job));
        let toast_height = (22.0 + text.size().y + 10.0).max(48.0);
        if i > 0 && y + toast_height > bottom {
            break;
        }
        let toast_rect =
            egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(toast_width, toast_height));
        y += toast_height + spacing;

        // An area the size of the toast, so a click on it lands on the toast
        // — and puts it away — rather than on a control it covers, unseen.
        // By the notification's own id: by its place, a toast arriving
        // between a press and its release took the pressed one's place, and
        // the release put the new one away.
        egui::Area::new(egui::Id::new(("notification", notif.id)))
            .fixed_pos(toast_rect.min)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let (rect, body) =
                    ui.allocate_exact_size(toast_rect.size(), egui::Sense::click());
                let painter = ui.painter();

                // Background
                painter.rect_filled(
                    rect,
                    6.0,
                    egui::Color32::from_rgba_unmultiplied(20, 20, 30, 230),
                );

                // Left accent bar
                let accent_color = notification_color(notif.level);
                let bar_rect = egui::Rect::from_min_size(rect.min, egui::vec2(4.0, rect.height()));
                painter.rect_filled(
                    bar_rect,
                    egui::CornerRadius {
                        nw: 6,
                        sw: 6,
                        ne: 0,
                        se: 0,
                    },
                    accent_color,
                );

                // Level label
                painter.text(
                    egui::pos2(rect.left() + 12.0, rect.top() + 8.0),
                    egui::Align2::LEFT_TOP,
                    notif.level.label(),
                    egui::FontId::proportional(10.0),
                    accent_color,
                );

                let elided = text.elided;
                painter.galley(
                    egui::pos2(rect.left() + 12.0, rect.top() + 22.0),
                    text,
                    egui::Color32::from_gray(220),
                );

                // Progress bar (fade out indicator)
                let progress_width = rect.width() * (1.0 - notif.progress);
                let progress_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.left(), rect.bottom() - 2.0),
                    egui::vec2(progress_width, 2.0),
                );
                painter.rect_filled(progress_rect, 0.0, accent_color.linear_multiply(0.5));

                // Dismiss button ("x"), over the body
                let dismiss = ui.put(
                    egui::Rect::from_min_size(
                        egui::pos2(rect.right() - 24.0, rect.top() + 4.0),
                        egui::vec2(20.0, 18.0),
                    ),
                    egui::Button::new(
                        egui::RichText::new("x")
                            .size(12.0)
                            .color(egui::Color32::GRAY),
                    )
                    .frame(false),
                );
                let body = if elided {
                    body.on_hover_text(&notif.message)
                } else {
                    body
                };
                if body.clicked() || dismiss.clicked() {
                    actions.session.notifications_to_dismiss.push(notif.id);
                }
            });
    }

    // Request repaint to animate progress bars
    ctx.request_repaint();
}
