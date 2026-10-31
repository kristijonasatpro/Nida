//! Responsive status while background GPU work and recording files finish.

use super::UIRunner;

impl UIRunner {
    pub(super) fn paint_waiting(&mut self, message: &str) {
        let Some(window) = self.window else { return };
        let Some(state) = self.egui_state.as_mut() else {
            return;
        };
        if self.cached_screen_size.width == 0 || self.cached_screen_size.height == 0 {
            return;
        }
        let input = state.take_egui_input(window);
        let mut output = self.egui_ctx.run_ui(input, |ui| {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("Nida");
                    ui.add_space(12.0);
                    ui.spinner();
                    ui.label(message);
                });
            });
        });
        state.handle_platform_output(window, output.platform_output);
        let jobs = self
            .egui_ctx
            .tessellate(output.shapes, output.pixels_per_point);
        if let (Some(gpu), Some(renderer)) = (&self.gpu, &mut self.egui_renderer) {
            for (id, deltas) in &output.textures_delta.set {
                for delta in deltas {
                    renderer.update_texture(&gpu.device, &gpu.queue, *id, delta);
                }
            }
        }
        self.present_ui(&jobs, output.pixels_per_point);
        if let Some(renderer) = &mut self.egui_renderer {
            for id in &output.textures_delta.free {
                renderer.free_texture(id);
            }
        }
        output.textures_delta.clear();
    }
}
