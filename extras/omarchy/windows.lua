-- Calepin: the notes window floats on the "notes" special workspace.
-- Only the notes window: its title starts with "Calepin" (detached tabs open
-- normal windows titled "file — Calepin"). Hyprland matches the whole title.
o.window({ class = "^io\\.github\\.antoineart\\.Calepin$", title = "^Calepin.*$" }, {
  float = true,
  center = true,
  size = { "(monitor_w*3/5)", "(monitor_h*22/25)" },
  workspace = "special:notes",
})
