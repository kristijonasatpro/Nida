//! Keep UI-owned navigation live when the show thread has no frame to lend.
//!
//! Engine actions remain ordered and bounded. A full queue applies backpressure
//! before egui consumes more input; no accepted command is overwritten.

use super::UIRunner;
use crate::usecases::ui::{UIActions, UIData, UILayoutState};
use std::time::Duration;

pub(super) const MAX_PENDING_FRAMES: usize = 32;
// Even a 2 ms timed wait can round up to a Windows scheduler tick. A retained
// snapshot makes waiting unnecessary: take the engine only when it is free.
const UI_ENGINE_PATIENCE: Duration = Duration::ZERO;

/// Keep interaction at roughly 60 Hz even when a 25/30 fps show, or a busy
/// engine, supplies fewer wakeups. With no interaction, retain idle pacing.
pub(super) fn interaction_wait(interacting: bool, since: Option<Duration>) -> Option<Duration> {
    interacting
        .then(|| Duration::from_micros(16_667).saturating_sub(since.unwrap_or(Duration::MAX)))
}

impl UIRunner {
    pub(super) fn with_ui_engine<R>(
        &mut self,
        what: &'static str,
        f: impl FnOnce(&mut Self) -> R,
    ) -> Option<R> {
        self.with_engine_for(what, UI_ENGINE_PATIENCE, f)
    }

    pub(super) fn drain_ui_actions(&mut self) {
        while let Some(actions) = self.stashed_actions.pop_front() {
            if let Some(shown) = self.ui_after(actions) {
                self.last_shown = Some(shown);
            }
        }
    }

    /// Resolve the last remaining positional engine action while the displayed
    /// snapshot still names its target. Reordering/removal during a busy period
    /// must never make a later click remove a different channel.
    pub(super) fn queue_ui_actions(&mut self, mut actions: UIActions, data: &UIData) {
        if let Some(index) = actions.session.remove_channel.take() {
            if let Some(channel) = data.channels.iter().find(|ch| ch.ch_idx == index) {
                actions
                    .commands
                    .push(crate::engine::EngineCommand::RemoveChannel {
                        channel_uuid: channel.uuid.clone(),
                    });
            }
        }
        self.stashed_actions.push_back(actions);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn runner() -> UIRunner {
        UIRunner::new(crate::app::AppConfig::parse_from(["varda", "--headless"]))
    }

    #[test]
    fn interaction_has_a_deadline_even_without_an_engine_wakeup() {
        assert_eq!(interaction_wait(false, None), None);
        assert_eq!(interaction_wait(true, None), Some(Duration::ZERO));
        assert_eq!(
            interaction_wait(true, Some(Duration::from_millis(30))),
            Some(Duration::ZERO)
        );
        assert_eq!(
            interaction_wait(true, Some(Duration::from_millis(10))),
            Some(Duration::from_micros(6_667))
        );
    }

    #[test]
    fn local_navigation_updates_a_retained_snapshot_without_changing_engine_values() {
        let mut data = UIData::test_fixture();
        let volume = data.channels[0].opacity;
        let mut layout = UILayoutState::default();
        let mut actions = UIActions::new();
        actions.session.select_channel = Some(1);
        actions.session.toggle_right_panel = true;
        actions.session.toggle_library_panel = true;
        layout.apply_selections(&actions);
        update_layout(&mut data, &layout);
        assert_eq!(data.selected_channel, Some(1));
        assert!(!data.right_panel_open);
        assert!(!data.library_panel_open);
        assert_eq!(data.channels[0].opacity, volume);
    }

    #[test]
    fn queued_removal_keeps_the_displayed_uuid_not_a_later_position() {
        let mut runner = runner();
        let data = UIData::test_fixture();
        let uuid = data.channels[0].uuid.clone();
        let mut actions = UIActions::new();
        actions.session.remove_channel = Some(data.channels[0].ch_idx);
        runner.queue_ui_actions(actions, &data);
        let actions = runner.stashed_actions.pop_front().unwrap();
        assert!(actions.session.remove_channel.is_none());
        assert!(matches!(&actions.commands[0],
            crate::engine::EngineCommand::RemoveChannel { channel_uuid } if *channel_uuid == uuid));
    }

    #[test]
    fn accepted_engine_actions_remain_in_order_across_busy_frames() {
        let mut runner = runner();
        let data = UIData::test_fixture();
        for _ in 0..MAX_PENDING_FRAMES {
            let mut actions = UIActions::new();
            actions
                .commands
                .push(crate::engine::EngineCommand::AddChannel);
            runner.queue_ui_actions(actions, &data);
        }
        assert_eq!(runner.stashed_actions.len(), MAX_PENDING_FRAMES);
        assert!(runner.stashed_actions.iter().all(|actions| {
            matches!(
                actions.commands.as_slice(),
                [crate::engine::EngineCommand::AddChannel]
            )
        }));
    }

    #[test]
    fn deferred_engine_edits_are_applied_once_and_in_order() {
        let gpu = crate::renderer::GpuContext::new_headless().expect("a GPU");
        let config = crate::testing::headless_config();
        let mut runner = UIRunner::new(config.clone());
        runner.egui_renderer = Some(egui_wgpu::Renderer::new(
            &gpu.device,
            wgpu::TextureFormat::Rgba8Unorm,
            egui_wgpu::RendererOptions::default(),
        ));
        runner.varda = Some(Box::new(crate::app::VardaApp::new(gpu, &config).unwrap()));
        runner.frame_order = crate::usecases::ui::state::ChannelOrder::of(
            runner.varda.as_ref().unwrap().mixer_ref().channels(),
        );
        let data = UIData::test_fixture();
        for value in [0.2, 0.8] {
            let mut actions = UIActions::new();
            actions
                .commands
                .push(crate::engine::EngineCommand::SetCrossfader(value));
            runner.queue_ui_actions(actions, &data);
        }
        runner.drain_ui_actions();
        assert!(runner.stashed_actions.is_empty());
        assert_eq!(runner.varda.as_ref().unwrap().mixer_ref().crossfader(), 0.8);
        runner.drain_ui_actions();
        assert_eq!(runner.varda.as_ref().unwrap().mixer_ref().crossfader(), 0.8);
    }

    #[test]
    fn busy_engine_does_not_hold_up_a_cached_ui_frame() {
        use super::super::engine_thread::EngineThread;
        use crate::{app::VardaApp, renderer::GpuContext};
        let gpu = GpuContext::new_headless().expect("GPU required for busy-engine regression");
        let config = crate::testing::headless_config();
        let app = Box::new(VardaApp::new(gpu.clone(), &config).unwrap());
        let mut runner = UIRunner::new(config);
        runner.engine = Some(EngineThread::start(app, gpu, || {}));
        // Hold the actual baton: the engine cannot make it available until
        // the test returns it, independent of GPU load or timing sleeps.
        let app = runner.engine.as_ref().unwrap().baton().take().unwrap();
        runner.cached_ui_data = Some(UIData::test_fixture());
        let start = std::time::Instant::now();
        let legacy = runner.with_engine("legacy admission baseline", |_| panic!("engine is held"));
        let legacy_wait = start.elapsed();
        assert!(legacy.is_none());
        let start = std::time::Instant::now();
        let fresh = runner.with_ui_engine("responsiveness test", |_| panic!("engine is held"));
        let waited = start.elapsed();
        assert!(fresh.is_none());
        let mut data = runner.cached_ui_data.take().unwrap();
        runner.layout.right_panel_open = false;
        update_layout(&mut data, &runner.layout);
        assert!(!data.right_panel_open);
        runner.engine.as_ref().unwrap().baton().put(app);
        drop(runner.engine.take());
        eprintln!("busy engine: legacy admission {legacy_wait:?}; cached UI admission {waited:?}");
        assert!(
            waited < Duration::from_millis(50),
            "UI admission waited {waited:?}"
        );
    }
}

/// The engine snapshot can be old while the panel arrangement and selection
/// are current. These fields belong to the UI, so refreshing them needs no
/// engine access and does not pretend an engine mutation has completed.
pub(super) fn update_layout(data: &mut UIData, layout: &UILayoutState) {
    data.selected_deck = layout.selected_deck;
    data.selected_channel = layout.selected_channel;
    data.selected_master = layout.selected_master;
    data.selected_sequence = layout.selected_sequence;
    data.selected_sequence_step = layout.selected_sequence_step;
    data.selected_macro.clone_from(&layout.selected_macro);
    data.stage_editor_open = layout.stage_editor_open;
    data.stage_editor_grid_size = layout.stage_editor_grid_size;
    data.stage_editor_snap = layout.stage_editor_snap;
    data.arrangement_mode_open = layout.arrangement_mode_open;
    data.arrangement_pixels_per_second = layout.arrangement_pixels_per_second;
    data.arrangement_scroll = layout.arrangement_scroll;
    data.arrangement_scroll_y = layout.arrangement_scroll_y;
    data.arrangement_snap = layout.arrangement_snap;
    data.arrangement_focus = layout.arrangement_focus.or_else(|| {
        data.transport
            .loop_region
            .map(|region| crate::usecases::ui::state::FocusRange::new(region.start, region.end))
    });
    data.library_panel_open = layout.library_panel_open;
    data.right_panel_open = layout.right_panel_open;
    data.screen.clone_from(&layout.screen);
    data.collapsed_channels
        .clone_from(&layout.collapsed_channels);
    data.dome_preview_open = layout.dome_preview_open;
    data.dome_mode_active = layout.dome_mode_active;
    data.dome_preset = layout.dome_preset;
    data.dome_geometry = layout.dome_geometry;
}
