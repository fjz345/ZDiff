use eframe::egui::{self, ScrollArea};
use zcommon::logger::LogBuffer;

pub fn log_window(ui: &mut egui::Ui, log_buffer: LogBuffer, scroll_to_bottom: &mut bool) {
    // Lock and clone logs for UI rendering
    let logs = {
        let buf = log_buffer.lock().unwrap();
        buf.clone()
    };

    // ScrollArea with vertical scrollbar and full size
    let scroll_response = ScrollArea::vertical()
        .auto_shrink([false; 2]) // Don't shrink smaller than contents
        .stick_to_bottom(*scroll_to_bottom)
        .show(ui, |ui| {
            // Fill available width & stretch height as needed
            ui.vertical(|ui| {
                for line in logs {
                    ui.label(line);
                }
            });
        });

    let has_scroll = scroll_response.content_size.y > scroll_response.inner_rect.size().y;

    if has_scroll {
        let scroll_pos = ui.ctx().input(|input| input.raw_scroll_delta.y);
        if scroll_pos > 0.0 {
            *scroll_to_bottom = false;
        } else if scroll_pos < 0.0 {
            *scroll_to_bottom = true;
        }
    } else {
        *scroll_to_bottom = true;
    }
}
