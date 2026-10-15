-- Nida on Omarchy. Load from ~/.config/hypr/hyprland.lua with: require("hypr.nida")
--
-- Nida itself opens each output window fullscreen on the monitor chosen in its
-- Outputs panel, and moves it back there when that monitor reconnects, so no
-- placement rules are needed. These keep Hyprland from getting in the way.

-- Omarchy starts a screensaver and then locks the screen after a few idle
-- minutes; on show day there is often no keyboard or mouse input for far longer.
o.window("^nida-output$", { idle_inhibit = "always" })

-- Output windows never take keyboard focus from the operator console. That also
-- keeps an output that is still waiting for its monitor opening on the operator's
-- screen, instead of on another output's fullscreen workspace.
o.window("^nida-output$", { no_initial_focus = true })
