-- Calepin stays resident: started hidden at login, SUPER+N shows it at once.
o.launch_on_start("sh -c 'command -v calepin >/dev/null && exec calepin --background'")
