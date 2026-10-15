//! LED feeds: an output carrying several LED screens at their own pixels of
//! one signal (see [`crate::engine::value::led`]).

use crate::engine::value::led::LedLayout;
use crate::engine::{CommandResult, ErrorCode};
use crate::renderer::context::OutputSource;

impl super::super::VardaApp {
    /// Make an output an LED feed carrying `layout`, or an ordinary output
    /// again with `None`. An output still opening takes it when it opens.
    pub fn cmd_set_output_led(
        &mut self,
        output_uuid: &str,
        layout: Option<LedLayout>,
    ) -> CommandResult {
        let layout = match layout.map(|l| self.checked_led(l)).transpose() {
            Ok(layout) => layout,
            Err(error) => return error,
        };
        if let Some(output) = self
            .output
            .outputs
            .iter_mut()
            .find(|o| o.uuid() == output_uuid)
        {
            output.set_led(layout);
            return CommandResult::Ok;
        }
        if let Some(config) = self
            .output
            .pending_output_creates
            .iter_mut()
            .find(|c| c.uuid == output_uuid)
        {
            config.led = layout;
            return CommandResult::Ok;
        }
        CommandResult::Err {
            code: ErrorCode::NotFound,
            message: format!("No output '{output_uuid}'"),
        }
    }

    /// Open a new output window carrying `layout`, fullscreen on `monitor`
    /// when one is named. Its name is "LED", or "LED 2" and on if taken.
    pub fn cmd_create_led_output(
        &mut self,
        monitor: Option<&str>,
        layout: LedLayout,
    ) -> CommandResult {
        use crate::scene::{OutputConfig, OutputTargetConfig};

        let layout = match self.checked_led(layout) {
            Ok(layout) => layout,
            Err(error) => return error,
        };
        let taken: Vec<String> = self
            .output
            .outputs
            .iter()
            .map(|o| o.name().to_string())
            .chain(
                self.output
                    .pending_output_creates
                    .iter()
                    .map(|c| c.name.clone()),
            )
            .collect();
        let name = free_name("LED", &taken);
        let mut config = OutputConfig::default_windowed();
        config.name = name;
        if let Some(monitor) = monitor {
            config.target = OutputTargetConfig::Display {
                name: monitor.to_string(),
            };
        }
        config.led = Some(layout);
        let uuid = config.uuid.clone();
        self.output.pending_output_creates.push(config);
        CommandResult::OkWithId { uuid }
    }

    /// Give the screen `screen_uuid` of an LED feed a bus of its own, named
    /// after the screen (with a number when taken) and as large as the
    /// master, and point the screen at it.
    pub fn cmd_create_led_screen_bus(
        &mut self,
        output_uuid: &str,
        screen_uuid: &str,
    ) -> CommandResult {
        let not_found = |message: String| CommandResult::Err {
            code: ErrorCode::NotFound,
            message,
        };
        let Some(output) = self
            .output
            .outputs
            .iter()
            .position(|o| o.uuid() == output_uuid)
        else {
            return not_found(format!("No output '{output_uuid}'"));
        };
        let Some(mut layout) = self.output.outputs[output].led().cloned() else {
            return not_found(format!("Output '{output_uuid}' is not an LED feed"));
        };
        let Some(screen) = layout.screens.iter_mut().find(|s| s.uuid == screen_uuid) else {
            return not_found(format!("No screen '{screen_uuid}' on that feed"));
        };
        let taken: Vec<String> = self
            .production_buses
            .iter()
            .map(|b| b.name.clone())
            .collect();
        let name = free_name(screen.name.trim(), &taken);
        let (width, height) = (self.render_width, self.render_height);
        let bus = match self.cmd_add_bus(&name, width, height) {
            CommandResult::OkWithId { uuid } => uuid,
            other => return other,
        };
        screen.source = OutputSource::BusProgram(bus.clone());
        self.output.outputs[output].set_led(Some(layout));
        CommandResult::OkWithId { uuid: bus }
    }

    /// `layout` with ids given to new screens, or why it cannot be shown.
    fn checked_led(&self, mut layout: LedLayout) -> Result<LedLayout, CommandResult> {
        let invalid = |message: String| CommandResult::Err {
            code: ErrorCode::InvalidInput,
            message,
        };
        if let Some(problem) = layout.problem() {
            return Err(invalid(problem));
        }
        for screen in &layout.screens {
            if let OutputSource::BusProgram(bus) = &screen.source {
                if self.production_buses.get(bus).is_none() {
                    return Err(invalid(format!(
                        "'{}' shows bus '{bus}', which does not exist",
                        screen.name
                    )));
                }
            }
        }
        layout.fill_in(crate::deck::generate_short_uuid);
        Ok(layout)
    }
}

/// `base`, or `base 2`, `base 3` … — the first not in `taken`.
fn free_name(base: &str, taken: &[String]) -> String {
    let free = |name: &String| !taken.contains(name);
    std::iter::once(base.to_string())
        .chain((2..=taken.len() + 2).map(|n| format!("{base} {n}")))
        .find(free)
        .unwrap_or_else(|| base.to_string())
}

#[cfg(test)]
mod tests {
    use crate::engine::value::led::{LedFit, LedLayout, LedScreen};
    use crate::engine::{CommandResult, ErrorCode};
    use crate::renderer::context::OutputSource;

    fn headless_app() -> Option<crate::app::VardaApp> {
        let gpu = crate::renderer::context::GpuContext::new_headless().ok()?;
        crate::app::VardaApp::new(gpu, &crate::testing::headless_config()).ok()
    }

    fn cube(source: OutputSource) -> LedScreen {
        LedScreen {
            fit: LedFit::Fill,
            ..LedScreen::new("Cube", [300, 300], [1920, 0], source)
        }
    }

    fn created(result: CommandResult) -> String {
        match result {
            CommandResult::OkWithId { uuid } => uuid,
            other => panic!("expected an id, got {other:?}"),
        }
    }

    #[test]
    fn a_new_led_feed_opens_as_a_window_carrying_its_screens() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let mut layout = LedLayout::single([1920, 1080]);
        layout.screens.push(cube(OutputSource::Master));
        let first = created(app.cmd_create_led_output(Some("HDMI"), layout.clone()));
        let second = created(app.cmd_create_led_output(None, layout));
        let pending = &app.output.pending_output_creates;
        assert_eq!(pending.len(), 2);
        assert_eq!(
            (pending[0].uuid.as_str(), pending[0].name.as_str()),
            (first.as_str(), "LED")
        );
        assert_eq!(pending[1].name, "LED 2", "a free name");
        assert!(matches!(
            &pending[0].target,
            crate::scene::OutputTargetConfig::Display { name } if name == "HDMI"
        ));
        let led = pending[1].led.as_ref().expect("the layout goes with it");
        assert_eq!(second, pending[1].uuid);
        assert!(led.screens.iter().all(|s| s.uuid.len() == 8), "ids given");
    }

    #[test]
    fn a_layout_is_checked_before_it_is_taken() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let out = created(app.cmd_create_led_output(None, LedLayout::single([1920, 1080])));
        let refused = |r: CommandResult, code: ErrorCode| matches!(r, CommandResult::Err { code: c, .. } if c == code);
        let mut bad = LedLayout::single([1920, 1080]);
        bad.screens[0].size = [0, 0];
        assert!(refused(
            app.cmd_set_output_led(&out, Some(bad)),
            ErrorCode::InvalidInput
        ));
        let mut ghost = LedLayout::single([1920, 1080]);
        ghost
            .screens
            .push(cube(OutputSource::BusProgram("nope".into())));
        assert!(refused(
            app.cmd_set_output_led(&out, Some(ghost)),
            ErrorCode::InvalidInput
        ));
        assert!(refused(
            app.cmd_set_output_led("missing", None),
            ErrorCode::NotFound
        ));
        // A good one reaches the output still opening; None clears it.
        let mut good = LedLayout::single([3840, 2160]);
        good.screens.push(cube(OutputSource::Master));
        assert!(matches!(
            app.cmd_set_output_led(&out, Some(good)),
            CommandResult::Ok
        ));
        let config = &app.output.pending_output_creates[0];
        assert_eq!(config.led.as_ref().map(|l| l.feed), Some([3840, 2160]));
        assert!(matches!(
            app.cmd_set_output_led(&out, None),
            CommandResult::Ok
        ));
        assert!(app.output.pending_output_creates[0].led.is_none());
    }

    #[test]
    fn an_led_feed_survives_being_saved_and_read_back() {
        let mut config = crate::scene::OutputConfig::default_windowed();
        let mut layout = LedLayout::single([3840, 2160]);
        layout.screens.push(LedScreen {
            fit: LedFit::Part {
                x: 10,
                y: 20,
                width: 300,
                height: 300,
            },
            ..cube(OutputSource::BusProgram("b1".into()))
        });
        layout.test_card = true;
        config.led = Some(layout.clone());
        let json = serde_json::to_string(&config).unwrap();
        let back: crate::scene::OutputConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.led, Some(layout));
        // An ordinary output writes nothing for it, and reads as none.
        config.led = None;
        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("\"led\""), "{json}");
    }
}
