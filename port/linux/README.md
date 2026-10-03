# Linux

`ninja linux` compiles the game with clang for 32-bit x86 Linux. The result
is a native executable, `build/linux/halo`. The game shows its graphics with
OpenGL 4.5. It plays sound through SDL3. It accepts keyboard, mouse and
gamepad input.

The game is 32-bit code because its data (tags, cache files, saved games)
contains 32-bit pointers, as on the Xbox.

## Requirements

You do not need the Xbox SDK. The declarations that the game uses are in
`port/include/xdk`.

To build:

- Python and ninja.
- clang. The option `--linux-cc` of `configure.py` selects a different
  compiler.
- The 32-bit glibc development files: `lib32-glibc` on Arch Linux,
  `gcc-multilib` and `libc6-dev-i386` on Debian and Ubuntu.
- The 32-bit SDL3: `lib32-sdl3` on Arch Linux, `libsdl3-dev:i386` on Debian
  and Ubuntu.
- For the large-scale mode (below), Rust with the 32-bit target
  (`rustup target add i686-unknown-linux-gnu`), and perl and make, with the
  32-bit `libgcc_s` and `libatomic` (`lib32-gcc-libs` on Arch Linux), which
  build the OpenSSL that the library carries. `configure.py` links the
  library when it finds `cargo`; `--large-mode=on` fails without it, and
  `--large-mode=off` builds the game without the mode.

To start the game:

- The 32-bit OpenGL libraries (`lib32-mesa`).
- The 32-bit PipeWire or PulseAudio client libraries (`lib32-pipewire` or
  `lib32-libpulse`).

## Build the game

1. Go to the root folder of the repository.
2. Enter `python configure.py`.
3. Enter `ninja linux`.

## Start the game

Enter `build/linux/halo`.

The game data is the folder that contains `maps/`, from an Xbox disc image
of any version of the game. The game looks for this folder in this
sequence:

1. `paths.data` in `config.toml`.
2. The current folder.
3. The folder of the executable.
4. `assets/` in the current folder, and `assets/` in the repository that
   contains the executable.

If the game finds no data, it asks for an Xbox disc image (`.xiso` or
`.iso`). This occurs at the first start:

- Select "No" to stop the game.
- Select "Yes" to open a file picker. Select the disc image. The game copies
  `maps/` next to the executable and shows the progress.

The game writes the copy to `maps.partial`. When the copy is complete, the
game changes the name to `maps`. If the copy stops before it is complete,
the game asks for the disc image again at the next start.

## Files and folders

| Xbox drive | Folder |
| --- | --- |
| `d:\` | The data root: the folder that contains `maps/`. |
| `z:\` | `z/` in the save root. This folder contains the cache (approximately 800 MB of map data) and the saved games. |
| `u:\` | `u/` in the save root. This folder contains the user data. |

The save root is `paths.saves` in `config.toml`. If that setting is empty,
the save root is `$XDG_DATA_HOME/halo-linux` (usually
`~/.local/share/halo-linux`).

The game makes the folders when it needs them. Names of files and folders
are not case-sensitive, as on the Xbox.

These files are in the data root:

| File | Contents |
| --- | --- |
| `debug.txt` | The log of the game. At start-up, the game shows the data root in the terminal. A crash writes its report (the faulting address and the calls that led to it) here as well; the `reference address` line at the top of each session places those addresses in the build. |
| `init.txt` | Console commands that the game does at start-up. For example, `map_name levels\a10\a10` starts the first campaign level. |

The settings are in `config.toml` next to the executable. Refer to
"Settings".

If the game stops because of a fatal signal, it writes the address and a
backtrace to the standard error. To find the function at the address, enter
`addr2line -e build/linux/halo <address>`.

## Controls

The keyboard and the mouse operate controller 1. The game adds the input of
the first gamepad to controller 1. The other gamepads operate controllers 2
to 4.

| Key | Controller | Function in the game |
| --- | --- | --- |
| W, A, S, D | left stick | move |
| mouse | (direct aim) | aim |
| left mouse button | right trigger | fire |
| right mouse button, G | left trigger | throw a grenade |
| space, enter | A | jump, accept |
| F, backspace, mouse button 4 | B | melee, back |
| E, R | X | action, reload |
| tab, mouse wheel | Y | change the weapon |
| Q | white | flashlight |
| X | black | change the grenade |
| left ctrl, C | left stick click | crouch |
| Z, middle mouse button | right stick click | zoom |
| arrow keys | D-pad | |
| escape | start | pause menu |
| F1 | back | |
| \` | | open the developer console |
| F12 | | release or capture the mouse |
| F11 | | change between fullscreen and window |

One movement of the mouse wheel changes the weapon one time. A second
movement after a short pause changes it again.

In the menus, the mouse moves a pointer:

- The item below the pointer gets the focus.
- A left click selects the item. On a setting with values, a click on the
  left or right half changes the value. On a button in the key of a screen
  (for example "B = Back"), a click pushes that button.
- A right click goes back.
- The mouse wheel moves through the items.

The keyboard also operates the menus. When the game continues, the mouse
aims again. A mouse button that you hold from the menu does not fire until
you push it again.

## Settings

The settings are in `config.toml` next to the executable
(`build/linux/config.toml`). At the first start, the game writes the file
with the default values and a comment for each setting. To get the default
values again, delete the file.

The game reads the file one time, at start-up. If a key is not correct, or
a value has the wrong type, the game writes the line to the log and uses the
default value.

Each setting has an environment variable. The environment variable changes
the setting for one start of the game. It has priority over the file.

| Setting | Default | Environment variable | Function |
| --- | --- | --- | --- |
| `display.fullscreen` | `true` | `HALO_FULLSCREEN` | `true`: fullscreen at the resolution of the display. The picture has 480 lines of the game and the width of the display. `false`: a window with the 640x480 picture of the Xbox. F11 changes between the two. |
| `display.window_scale` | `2` | `HALO_WINDOW_SCALE` | The size of the window, as a multiple of 640x480. You can change the size of the window. |
| `display.vsync` | `true` | `HALO_NO_VSYNC=1` sets `false` | `true`: each frame waits for the display. |
| `display.max_fps` | `0` | `HALO_MAX_FPS` | With vsync off, the most frames each second. `0`: twice the display's refresh rate. `-1`: no limit, which can hang some Intel graphics (Raptor Lake), resetting the desktop's graphics too. |
| `display.interpolation` | `true` | `HALO_INTERPOLATION` | `true`: one frame for each refresh of the display. `false`: 30 frames each second, as on the Xbox. Refer to "Frame rate". |
| `display.direct_camera` | `true` | `HALO_DIRECT_CAMERA` | `true`: in first person, on foot, the view points where the player aims in each frame, not where the last tick left it. Refer to "Frame rate". |
| `display.high_res_hud` | `true` | `HALO_HIGH_RES_HUD` | `true`: the HUD (meters, counters, panels and their outlines, the motion sensor, reticles, waypoints, scopes) is drawn from the high-res assets in `port/assets/hud`, 8x the size of the maps' bitmaps. The bitmaps with English text keep the maps' own. `false`: the maps' own bitmaps. |
| `display.high_res_text` | `true` | `HALO_HIGH_RES_TEXT` | `true`: the menus' and HUD's text is drawn with the fonts in `port/assets/fonts` (Overpass, in place of the maps' Interstate) at the display's resolution, laid out as before, and the menus' titles are drawn from the high-res pictures in `port/assets/titles`. `false`: the maps' bitmap fonts and titles. |
| `display.player_names` | `"all"` | `HALO_PLAYER_NAMES` | In multiplayer, whose names are drawn above their heads: `"all"`, `"allies"`, `"enemies"` or `"none"`. An ally's name is drawn above the triangle the game shows over teammates. An enemy's name shows only while the enemy is in sight and not camouflaged, so it never shows where an enemy hides, and only as far away as the weapon in hand turns its reticle red over an enemy (at least 20 world units, the motion sensor's reach, and at most 70). |
| `display.player_name_scale` | `1.0` | `HALO_PLAYER_NAME_SCALE` | How large the players' names are drawn: `1.0` is three quarters of the size of the HUD's text, from `0.25` to `4`. With high-res text, larger names are rasterized at their size, so they stay sharp. |
| `display.scoreboard_team_layout` | `"teams"` | `HALO_SCOREBOARD_TEAM_LAYOUT` | How the multiplayer scoreboard (hold BACK, or F1) lists a team game's players. `"teams"`: a column for each team, red on the left and blue on the right. `"score"`: all the players in order of score. With more players than fit, the mouse wheel and Page Up / Page Down scroll the scoreboard. |
| `display.scoreboard_background` | `true` | `HALO_SCOREBOARD_BACKGROUND` | `true`: the multiplayer scoreboard (hold BACK, or F1) has a panel behind its text, for clearer text. |
| `display.scoreboard_background_color` | `"16, 16, 16, 150"` | `HALO_SCOREBOARD_BACKGROUND_COLOR` | The colour of the scoreboard's panel: `"red, green, blue, alpha"`, each from `0` to `255`. Alpha `0` is see-through, `255` is solid. |
| `audio.enabled` | `true` | `HALO_NO_AUDIO=1` sets `false` | `false`: no audio device. The sound continues without output. |
| `audio.volume` | `1.0` | `HALO_VOLUME` | The master volume. |
| `input.mouse_sensitivity` | `1.0` | `HALO_MOUSE_SENSITIVITY` | The multiplier for the mouse aim. |
| `input.invert_mouse` | `false` | `HALO_MOUSE_INVERT=1` sets `true` | `true`: the vertical mouse aim is inverted. |
| `input.mouse_aim_assist` | `false` | `HALO_MOUSE_AIM_ASSIST` | `true`: the magnetism of the controller also operates for the mouse. `false`: when the mouse moved after the right stick, the view is not slowed or dragged by a target. The autoaim of the bullets operates in both cases. |
| `game.console_log` | `"important"` | `HALO_CONSOLE_LOG` | What the console shows on the screen. `"important"`: bans, players that the host drops for cheating, the reasons that the game refuses a command, and the asserts that stop the game. `"all"`: all the lines. `"none"`: only the asserts that stop the game. The output of a command always shows. `debug.txt` gets all the lines. |
| `game.language` | `""` | `HALO_LANGUAGE` | The language of the menus: `ja`, `de`, `fr`, `es` or `it`. Empty: English. |
| `paths.data` | `""` | `HALO_DATA_ROOT` | The data root. Refer to "Start the game". |
| `paths.saves` | `""` | `HALO_SAVE_ROOT` | The save root. Refer to "Files and folders". |
| `network.address` | `""` | `HALO_NET_ADDRESS` | The IPv4 address of this machine for system link. Refer to "Play on one computer". |
| `network.broadcast` | `""` | `HALO_NET_BROADCAST` | IPv4 addresses, with commas between them, that get the broadcasts of the game. Empty: 255.255.255.255. |
| `network.online` | `true` | `HALO_NET_ONLINE` | `true`: internet play. `false`: system link on the local network only. |
| `network.join_from_clipboard` | `true` | `HALO_NET_JOIN_FROM_CLIPBOARD` | `true`: when the game comes to the front, it joins the game of an invite link on the clipboard. |
| `network.tunnel_port` | `0` | `HALO_NET_TUNNEL_PORT` | The UDP port for internet play. `0`: the game selects a port. Refer to "Internet play". |
| `network.allow_upnp` | `true` | `HALO_NET_ALLOW_UPNP` | `true`: internet play can ask the router to forward its port (UPnP). `false`: the game does not ask. Refer to "Internet play". |
| `network.signalling_brokers` | three public brokers | `HALO_NET_BROKERS` | The public MQTT brokers (`host:port`, with commas between them) that let the machines of an invite find each other. |
| `network.stun_servers` | Google and Cloudflare | `HALO_NET_STUN` | The public STUN servers (`host:port`, with commas between them) that give the internet address of a machine. |
| `discord.application_id` | the application of the project | `HALO_DISCORD_APPLICATION` | The Discord application for invites. Empty: no Discord. |
| `update.auto` | `true` | `HALO_UPDATE_AUTO` | `true`: at start-up, the game looks for a new version. Refer to "Updates". `false`: the game does not look. |
| `debug.update_answer` | `""` | `HALO_UPDATE_ANSWER` | The answer to the update question, for automatic tests: `yes`, `no` or `never`. Empty: the game asks. |
| `debug.exit_after` | `0.0` | `HALO_EXIT_AFTER` | The game stops after this number of seconds. `0`: never. |
| `debug.screenshot_directory`, `debug.screenshot_every` | `""`, `0` | `HALO_SCREENSHOT_DIR`, `HALO_SCREENSHOT_EVERY` | The game writes each Nth frame to this folder as a BMP file. |
| `debug.hidden_window`, `debug.null_renderer` | `false` | `HALO_HIDDEN_WINDOW`, `HALO_NULL_RENDERER` | `true`: no visible window, or no graphics. |
| `debug.gpu_stats`, `debug.gpu_trace_frame`, `debug.gpu_trace_constants`, `debug.gpu_dump_shaders`, `debug.texture_dump_directory`, `debug.texture_log`, `debug.gl_debug`, `debug.texture_no_cache` | off | `HALO_GPU_STATS`, `HALO_GPU_TRACE`, `HALO_GPU_TRACE_CONSTANTS`, `HALO_GPU_DUMP_SHADERS`, `HALO_TEXTURE_DUMP`, `HALO_TEXTURE_LOG`, `HALO_GL_DEBUG`, `HALO_TEXTURE_NO_CACHE` | Tools to find problems in the graphics: counts for each frame, all the GL state of one frame, the GLSL code, the textures. |
| `debug.gpu_skip_vertex_shaders`, `debug.gpu_debug_expression`, `debug.gpu_debug_flat`, `debug.gpu_debug_texture0` | off | `HALO_GPU_SKIP_VS`, `HALO_GPU_DEBUG_EXPR`, `HALO_GPU_DEBUG_FLAT`, `HALO_GPU_DEBUG_T0` | Tools to find problems in the graphics: skip the draws of a vertex shader, or replace the output of all pixel shaders with a GLSL expression (for example `t0.rgb`). |
| `debug.network_test`, `debug.network_test_start`, `debug.network_test_kill`, `debug.network_test_score`, `debug.network_test_shoot`, `debug.network_test_vehicle`, `debug.network_test_pickup`, `debug.network_test_pickup_weapon`, `debug.test_input` | off | `HALO_NETWORK_TEST`, `HALO_NETWORK_TEST_START`, `HALO_NETWORK_TEST_KILL`, `HALO_NETWORK_TEST_SCORE`, `HALO_NETWORK_TEST_SHOOT`, `HALO_NETWORK_TEST_VEHICLE`, `HALO_NETWORK_TEST_PICKUP`, `HALO_NETWORK_TEST_PICKUP_WEAPON`, `HALO_TEST_INPUT` | Automatic tests of system link (`game/network_test.c`). Refer to `NETCODE.md`. |
| `debug.scenario`, `debug.scenario_trace` | `""` | `HALO_SCENARIO`, `HALO_SCENARIO_TRACE` | The comparison harness: plays a scenario file with the first player of a network test game, alone, and writes a trace of the player's state. `tools/scenario_harness.py` runs it. Refer to `tools/scenarios/README.md`. |
| `large.map`, `large.gateway`, `large.spacetimedb`, `large.database`, `large.root`, `large.name`, `large.log_players`, `large.scoreboard`, `large.autofire`, `large.autofire_cycle`, `large.automelee`, `large.autouse`, `large.log_sounds` | `""`, `127.0.0.1:7777`, `http://127.0.0.1:3000`, `""`, `""`, `""`, `false`, `false`, `false`, `0`, `false`, `0`, `false` | `HALO_LARGE_MAP`, `HALO_LARGE_GATEWAY`, `HALO_LARGE_SPACETIMEDB`, `HALO_LARGE_DATABASE`, `HALO_LARGE_ROOT`, `HALO_LARGE_NAME`, `HALO_LARGE_LOG`, `HALO_LARGE_SCOREBOARD`, `HALO_LARGE_AUTOFIRE`, `HALO_LARGE_AUTOFIRE_CYCLE`, `HALO_LARGE_AUTOMELEE`, `HALO_LARGE_AUTOUSE`, `HALO_LARGE_LOG_SOUNDS` | The large-scale mode. Refer to "Large-scale mode". |
| `debug.network_latency`, `debug.network_loss` | `0` | `HALO_NETWORK_LATENCY`, `HALO_NETWORK_LOSS` | The game holds all the data that it receives for this number of milliseconds, and ignores this percentage of the datagrams. Use these settings to test the netcode as on the internet. |
| `debug.telnet_console`, `debug.telnet_console_port` | `false`, `2323` | `HALO_TELNET_CONSOLE`, `HALO_TELNET_CONSOLE_PORT` | The game listens on 127.0.0.1, on this port, for a script console (connect with telnet). The console has no password, so only this computer can reach it. |

With Mesa drivers, the game sends its GL calls through the GL thread of
Mesa. To stop this, set the environment variable `mesa_glthread=false`.

## Large-scale mode

A mode for about 500 players in one match, on a dedicated server (a
SpacetimeDB with the match module, and a UDP gateway beside it:
`rust/halo-match-module`, `rust/halo-gateway`). The mode for up to 128 players
is the default and is not changed. The Rust library `rust/halo-client` is
linked into the game as a 32-bit static library (`ninja linux`; on Windows,
`ninja windows`; not on Android) and owns the match connection: the per-tick
state from the gateway over UDP, the slow state from SpacetimeDB directly. The
adapter between it and the game is `game/large_mode.c`.

Setting `large.map` (for example, `bloodgulch`) starts a session from the
settings, without the lobby: the game hosts a game of one machine on that map,
as `debug.network_test` does, and when the map is loaded the library connects to
`large.spacetimedb`, takes a seat in the match of the database `large.database`
(a new SpacetimeDB identity for the session; the match must have a map with
starting locations for its game, and room), and joins the gateway at `large.gateway` over UDP as the player the
seat is, proving it with an Ed25519 key pair whose public key is on the seat
(`halo-wire`'s `auth`). The player leaves the match when the game ends.
In this mode the distributed netcode does not run (`network_distributed.c`
ignores its tick and its messages), and the other players are what the gateway
sends, held by the library. The game plays Team Slayer, as the mode has two
teams.

The other players are drawn by the existing renderer. For each player the
gateway sends, who is on the match's roster (a name and a team, which come over
the direct connection: the `roster` table of the match module), the adapter
makes a biped in the team's colour, with the engine's physics suspended: each
tick the engine is given the controls of a player running at the velocity the
server gave, which makes it choose and play the running animation, and the unit
is put where the server has the player, facing as it faces. The engine's player
records hold 128 players in all, so only 127 of the remote units have a player.
The units nearest the local player have them, and a few are swapped each second
as players move; the others are units alone, drawn all the same. Anything else
that reads the player records still sees the nearest 127. The HUD does not:
for a unit with no player, it takes the name over the head and under the
crosshair, the friendly marker and the team from the adapter's own record of
the unit (the match's roster: `large_mode_bare_remote_unit`), so that it looks
the same as one with a player, under the same rules of range and sight. The
motion sensor already reads a unit's team and velocity (the adapter writes the
server's after the objects are updated), so it shows both alike.
A player the gateway has not sent a state of for 120 ticks is out of range, and
one who has left the match is gone at once: the unit is deleted, and made
afresh when the gateway sends the player again.

Between the gateway's updates a player is not left where the last state put
them: the library draws each remote player where that state would have taken
them by now (its velocity carried forward by the ticks since it came, and
gravity down to the map's ground for a player in the air), and the adapter puts
the unit there and hands the engine that velocity. It goes on for at most the
planner's staleness cap (15 ticks), after which the player is held still, with
no velocity, so that the engine shows them standing. A state that arrives where
the extrapolation was not leaves an offset that shrinks by 0.6 each tick; one
more than 2 world units away is where the player is drawn at once (a respawn).
With `large.log_players`, the game logs each new state's drawn error, the
distance between where the player was drawn and where the state puts them
(`large mode: drawn error player 7 tick 812 0.0123 (x y z)`); `smoothness.py`
reports their median, p99 and largest by band.

Each second, the game logs one line for the session (`large mode: tick ...`:
whether the gateway has welcomed the player and the direct connection is
up, and what has been received) and, with `large.log_players`, a line for
each player the gateway has sent (`large mode: player 7 tick 812 (x y z) v
(...) yaw .. pitch ..`), and one for the remote units (how many there are, how
many have players, and what the adapter cost a tick over the last second).
With `large.log_players`, a line also says where the engine has each remote
unit (`large mode: drawn 7 tick 812 (x y z) team 1 player 3`, with the tick of
the state it was driven from). Another says what the HUD makes of each (`large
mode: hud 7 mine 0 bare 1 bare_team 1 sensor 1 blip 2 named 1 name Foo`: the
local player's team, whether the unit has no player, whether the motion sensor
reaches it and the type of its contact, and the name that aiming at it shows),
which the automated test compares with the roster. The local player stands
where the server has them, and tells the gateway where they are.

### Spawning, deaths and the score

The server owns them: the game's player is only where the server has put them.
The match module (`rust/halo-sim`'s `rules`) spawns a player who joins at a
starting location of the map by the engine's rules, or, when none is free,
beside one, at once; only a player who can be put in neither place is told which
respawn wave they are waiting for and spawned in it. The match's public tables `standing` (each player's score, deaths, whether
they are in the world and when they will be) and `game_state` (the game, its
limits, the team scores and how it ended) come over the direct connection, as
the roster does.

- The engine makes the local player's unit only while the server says the
  player is alive (the gate in `game_engine_should_spawn_player`, and
  `find_best_starting_location_index`, which need not choose: the starting
  locations are taken by the other players' units), and the adapter puts it
  where the server spawned the player, facing as it says, at each spawn.
- When the server says the player is dead the unit is killed, and the HUD says
  `You will respawn in N seconds`; when it says the player waits for a wave,
  `No spawn point is free: respawn wave in N seconds` (the engine's own
  respawn timer and death message are not used: `game_engine_player_killed`
  does nothing in the mode). Another player the server says is dead is not
  drawn.
- The scoreboard (hold the score button; it stays up when the game has ended,
  with the winner in its title, until the server's next match is joined)
  lists every player of the match from `standing`, in range or not, by score,
  with each player's kills and deaths, in a column for each team in Team
  Slayer, and scrolls as the engine's does (`game_engine_rasterize_large_scoreboard`).
  `large.scoreboard` keeps it up all the time, for pictures.

Each change of the local player's life is logged (`large mode: the server says
the local player is waiting|alive|dead (spawns, score, deaths, team, tick,
due)`), as is the unit being put where the server spawned the player.

The test of the mode runs the game headless against a local server:

```
HALO_STDB_BIN=<SpacetimeDB 2.10.x directory> HALO_MAP_DIR=<folder of the .map files> \
HALO_GAME_BIN=build/linux/halo HALO_DATA_ROOT=<folder that contains maps/> \
cargo test --release --manifest-path rust/halo-client/Cargo.toml --test headless -- --nocapture
```

It starts its own SpacetimeDB and gateway on Blood Gulch with 120 simulated
seated players walking (`HALO_HEADLESS_PLAYERS=500` for 500), runs the game for
50 seconds in a hidden window, and compares every position the game logged,
for the library and for the engine's units, with the server's. A player
leaves the match and joins it again, to see the unit go and come back. With
`HALO_SCREENSHOT_DIR` and `HALO_SCREENSHOT_EVERY` the game saves frames, and
`HALO_HEADLESS_LOG` keeps its log. Without the data, it skips.
A second test (`--test rules_headless`) runs a Slayer match of `halo-server` on
the real Blood Gulch with 40 simulated players beside the game: the game is
spawned at once, killed and respawned by the server, and sees the match end at its score limit with the final scoreboard and the rotation move
on.
The library's own tests (`rust/halo-client/tests/boundary.rs` and
`servers.rs`) need only `HALO_STDB_BIN`.

### Fighting

The weapon and the damage are the server's too, by the weapon's tags: a
player spawns with the multiplayer pistol (the engine's own starting weapon in
a game without teams is the plasma pistol, which the weapon model has: it
charges, heats and ages like the engine's; what a player starts with is the
match's choice), and the match module keeps each player's shield, health and
weapons in the public table `fighter` (the shield is kept as of a tick and
counted forward by the client and the server, so a recharge writes nothing).

- The game's weapons fire in the engine as they do offline. The engine's own
  damage to another player's unit is switched off in the mode
  (`large_mode_damage_deals`); in its place, when the local player's shot
  hits a remote player's unit, the game reports the hit to the server
  (`halo_large_report_hit`), once a tick in a batch of at most 64, with the
  damage (the damage effect's tag, which says what hurt the player: a bullet,
  an explosion or a melee blow, and so which weapon's it is), the part of the
  body, the scale the engine dealt it at (how far a bullet had flown, how far
  the player was from the blast, how fast the blow was struck), where it hit and
  where the shooter saw the target. The report goes in the reliable reducer
  `report_hits` over the direct connection and not in a datagram: a hit lost on
  the way would be a kill lost, and the connection says who shot.
- The server judges each report (`halo_sim::combat::resolve`: finite numbers,
  both players alive and in the match, not oneself, a weapon carried (or put
  down in the last 15 seconds) that has the damage, not older than 3 seconds
  nor from the future, the impact at the target, no more hits than the
  weapon fires (a pellet of a shotgun's shot is a hit, a blast that hurts
  several is one), the shooter within the weapon's reach, the target near where
  the shooter saw it). A report that fails is dropped, counted against the
  shooter (the private table `shooter`) and in `match_tick.rejected_hits`, and
  logged by `halo-server` (`rejected hits N (+M)`, with `hits accepted` and how
many were refused as `TargetNotWhereSeen`, from `match_tick.hits_total` and
`match_tick.rejected_not_where_seen_total`). A hit that passes deals the
  damage to the target's shield and health as the engine does, rolled by the
  server at the report's scale brought down to what the server's view allows
  (`halo_sim::source::limit`); the hit that takes the last health is a death
  for the rules, with the shooter as the killer, who scores.
- The game shows the server's shield and health on the local HUD (the shield
  flash and the recharge are the engine's own, driven by the table), and a
  remote player the hits killed falls and stays as a body.
- `large.autofire` (`HALO_LARGE_AUTOFIRE`) aims the local player at the nearest
  remote player and holds the trigger: the tests and the pictures use it.
  `large.autofire_cycle` lets go of it for a tick after that many, for a weapon
  that fires when the trigger is let go (the plasma pistol's charge).
  `large.automelee` (`HALO_LARGE_AUTOMELEE`) strikes a blow every 40 ticks
  instead.

### The local player's view: weapon, HUD, sounds and effects

- The local weapon is fired by the engine (its effects, sounds and first-person
  animation are the tags'); its rounds, heat and reload are `halo_sim`'s
  `Hands`, kept by the library (`halo_large_fire`, once a tick), and put into
  the engine's weapon before the HUD reads it, so the HUD's ammunition is the
  library's. The game logs both once a second, with how many shots the engine's
  weapon and the library's model have each counted (they agree).
- Remote firing is in the state the gateway sends: bits 2 to 4 of a unit's flags
  count the shots the player's weapon has fired (modulo 8) and bit 5 says it is
  reloading (`halo-wire`'s `unit.rs` has the reasoning, and the bandwidth: none
  added). Every remote player is given the weapon the server says they carry
  (the `fighter` table's loadout), drawn in their hand; each shot the count
  tells of is fired by the engine's weapon, so the muzzle flash, the projectile
  and its impacts, the shell and the sounds are the weapon's own tags', for any
  weapon. `port/linux/game/large_effects.c` is the engine's side (weapons,
  being hurt, the killing blow, sounds).
- A hit the server's table gains for a player is shown as the engine shows one:
  the unit's pain sound and flinch, and for the local player the screen's flash
  and shake, the damage sound and the direction of the hit, from the damage
  effect of the shooter's weapon's projectile. A killing blow is that damage
  too, so the death sounds as a shot player's does.
- `large.log_sounds` (`HALO_LARGE_LOG_SOUNDS`) logs each sound the game is asked
  to start with its tag and the game tick; the tests read what played from it.
  The game also logs the frames a second it draws once a second.

`rust/halo-client/tests/effects_headless.rs` runs the real game for these: the
HUD against the library over a magazine, a reload and a kill; a simulated player
on the wire firing, hitting and killing another and the game's player, with the
sounds each causes; and a hundred players in view (the frame rate, with
`HALO_FPS_FLOOR`).

`rust/halo-client/tests/combat_headless.rs` runs real games: the game
shoots a simulated player until it dies, with the pistol, the plasma rifle, the
plasma pistol (tapped and charged), the rocket launcher, the needler and a melee
blow; simulated players shoot the game's player (shield down, recharge,
death, respawn); a bystander watches one simulated player kill another. Run it as `headless` above, with
`--test combat_headless -- --test-threads=1`.

### Items and pickups

The server owns the items on the ground (`halo_sim::items`, `halo_sim::pickups`),
and the game only shows them. The map's netgame equipment places them: each
placement that lists the game makes one of its item collection's items (by
weight) when the match's time is a multiple of its period (its own spawn time,
else its collection's, else 30 seconds; the first tick of a match is time 0), and
takes the one nobody took away as it does. Overshield, active camouflage, health
packs and the weapons are made; the grenades are the grenades' ticket's.

- The public table `item` has a row for each item: where it is and how it moves
  (world units a tick) as of a tick, whether it is at rest, which placement made
  it, a weapon's rounds. A row is written when an item appears, when it comes to
  rest, when it loses rounds and when it goes: not while it falls. Where a
  falling item is in between is a function of its row, which the server's tick
  and every client work out with the same code (`Item::advanced_to`, the
  engine's `item_update` against the map's collision data), so the weapon a
  player sees fall is the server's, to the bit; the library's `halo_large_items`
  does it once a frame. The public tables `powerup` (who is camouflaged, and
  until which tick) and `kit` (the rounds a player's weapons have: a client
  subscribes to its own row) are small.
- The game makes an engine object for each item (`large_mode_update_items`),
  at rest as far as the engine is concerned, and puts it where the library says;
  the engine's own item spawning, purging and pickups are off in the mode
  (`game_engine_update_item_spawn`, `game_engine_update_purge`,
  `players_decide_pickups`), and an item the engine made that the server did not
  (the weapons of a unit that was killed) is swept away.
- The player's action button is a press the game tells the server
  (`halo_large_use`, the reliable reducer `use_item`, with the weapon slot in
  hand); the server decides what the player takes: nearest first, and one gets
  an item, whoever reaches for it on the same tick (the nearer, then the lower
  id). Powerups, ammunition of a weapon held and a weapon with nothing in hand
  need no press. The weapons the player carries are the server's
  (`large_mode_sync_weapons`); the rounds the engine counts as it fires are told
  to the server every so often (`halo_large_report_ammo`).
- A dead player puts down every weapon and loses their camouflage; a weapon
  swapped for another is put down too. Dropped weapons last 30 seconds.
- `large.autouse` (`HALO_LARGE_AUTOUSE`) presses the action button twice a
  second from that many seconds after the weapon is in hand: the tests use it.

`rust/halo-client/tests/items_headless.rs` runs two real games (`headless`
above, with `--test items_headless -- --test-threads=1`).

### Pick a server from the list

With `large.root` set (the name of the server list's root database, such as
`halo-root`) and `large.map` empty, the game does not host a map of its own:
the player picks a server. `large.spacetimedb` is the SpacetimeDB the server
list is on (the operator tells players its address). The original game's menus
are tags of the game's data, and the mode adds none, so the list is on the
developer console (push \` to open it, in the main menu or in a game), which
runs three commands here:

| Command | |
| --- | --- |
| `servers` | Lists the servers: each one's map, game type and players (`3/200`), and who you are (your identity, which a ban names). A `*` marks the server you are on. |
| `join <number>` | Takes a seat on that server of the list (or `join <id>`). When the server has given a seat, the game hosts a game of one machine on the server's map and the library brings the other players. Joined from a game, the game of the server you were on ends first. |
| `leave` | Stops being on the server. |

A server that is full says so, and one that has banned you says why
(`you are banned from this server: <reason>`), on the console. A full server
is asked again for 20 seconds. On a server the game follows its rotation:
when the list names another match, the game ends, shows its scores and joins
the new one, on its map, by itself.

The player's identity is kept between sessions in the save root
(`u/large_identity/`, one file for each SpacetimeDB; whoever holds the file is
that identity). It is the same on the server list's root database and on every
match, so it follows the player from server to server and from session to
session, and it is what bans name. A SpacetimeDB gives and checks its own
tokens, so another SpacetimeDB is another identity, unless the two share their
signing keys. A token the server no longer accepts is replaced by a new
identity.

`rust/halo-client/tests/server_list.rs` runs this with the real game: a whole
server on the real maps, the game's console (through the telnet console, which
runs the same commands), a join, the rotation, a second server and a ban. It
needs the same data as the headless test above.

### Running a server

`halo-server` (`rust/halo-server`) runs everything of a server from one
configuration file: SpacetimeDB (started and stopped by it, or one that is
running), the server list (a root database, `rust/halo-root-module`), and for
each server of the list a rotation of matches, each a database of its own made
from the match module, with a gateway in front of it. You supply the maps (your
own copy of the game's data). Build the two modules and the program, and start
it:

```
(cd rust/halo-root-module && cargo build --release --target wasm32-unknown-unknown)
(cd rust/halo-match-module && cargo build --release --target wasm32-unknown-unknown)
cd rust/halo-server && cargo build --release
target/release/halo-server --example > server.toml     # edit it
target/release/halo-server --config server.toml
```

`server.toml` sets the SpacetimeDB to use, the modules, the maps folder and, for
each server, its rotation: the map, game type, variant, player cap and length of
each match and the bytes a second each player may be sent. The example documents
every setting. A match ends when its time is up, and the next of the rotation,
made ready a few seconds before, takes over: the list names it and the players'
games follow. The old match's gateway stops and its database is deleted. Each
server's log line (every `log_secs`) says the tick time (the server's own
metrics, whole and the module's part), the players against the cap, the
bandwidth sent, the moves rejected, late inputs and the gateway's send time.

Open the UDP ports of `bind` and the next one for each server, and run it under
a supervisor that restarts it. `halo-server ban <identity> [reason]`,
`unban <identity>`, `bans` and `servers` act on the running server's root
database: a ban takes the player out of every running match and refuses them
the next, with the reason.

## Updates

The builds from GitHub Actions (refer to the main [README](../../README.md#download))
can update themselves. At start-up, the game asks GitHub for the latest
release. The game does not wait for the answer. If the latest release is not
newer, the game does nothing.

If the latest release is newer, the game asks: "Do you want to update?"

- Select "Yes" to update. The game downloads the release for this platform,
  replaces its files and starts the new version. The old files get the
  extension `.old`. The new version deletes them.
- Select "No" to continue. The game asks again at the next start.
- Select "Do not ask again", then "Yes", to stop the questions. The game
  writes `auto = false` in the `[update]` section of `config.toml`. To get
  the questions again, set `auto = true`.

The game downloads through HTTPS. It examines the certificate of the server
against the certificate authorities of the system: on Linux, the bundle of
the distribution (`src/posix_update.c`, with Mbed TLS); on Windows, the
certificate store of Windows (WinHTTP). The folder of the executable must
let the game write to it.

Builds that you make yourself have no build number. They do not look for
updates.

## Frame rate

The game calculates its world at 30 Hz, as on the Xbox. On the Xbox, the
game showed one frame for each calculation (tick). This port shows one frame
for each refresh of the display, for example at 60, 120 or 240 Hz.

Each frame shows the world between the last two ticks
(`game/render_interpolation.c`):

- After each tick, the game keeps the camera, the position of each part of
  each object, and the first-person weapon.
- Each frame mixes the last two ticks. The mix agrees with the time since
  the last tick.
- Rotations use quaternions. Positions and scales are linear.
- A teleport, a respawn or a cut of the camera does not mix. It jumps.

Thus the frames are one tick (33 ms) after the calculation. The calculation
does not change.

The direction of the view is an exception. The game reads the mouse and the
sticks in each frame. In first person, on foot, each frame points the view
where the player aims at that time (`display.direct_camera`). Thus the view
turns in the frame that the mouse moves. In a vehicle and in cinematics, the
view mixes as the other things do. On Android, the view mixes as before.

To get 30 frames each second, set `display.interpolation = false`.

To see the frame rate:

1. Push \` to open the developer console.
2. Enter `display_framerate true`.

In the game of another host, the console runs only the commands that change
nothing of the game (such as `display_framerate`), and the game puts back
cheats, the game speed and the settings of the drawing that show more of
the world (such as `rasterizer_wireframe`). Refer to `NETCODE.md`.

The frame rate shows at the bottom right of the screen. It is the mean over
half a second.

## System link

The Xbox game lets 16 players on 4 machines play a system link game. This
port lets up to 128 players on up to 128 machines play. Each machine can
have up to 4 players (split screen).

- `include/halo_port_limits.h` sets the limits.
- `include/halo_port_capacity.h` sets the memory for the limits. The game
  state is 16 MB at `0x81A00000` (3.3 MB on the Xbox). The pools of objects,
  effects, particles, contrails, lights and sounds are also larger.

Obey these rules:

- All the machines in a game must use a build with the same limits.
- The port uses protocol version 2. It does not see the Xbox game or older
  builds of the port. They do not see the port.

These are the differences from the Xbox:

- The host waits up to 60 seconds (15 seconds on the Xbox) for the other
  machines to load the map.
- If a machine does not read the messages of the host for two seconds, the
  host removes it from the game.
- The saved games contain all the game state. Thus a saved game is 16 MB.
  Saved games from older builds of the port do not operate.
- In campaign and in games of up to 16 players, the game removes garbage
  (bodies, dropped weapons) as on the Xbox. In larger games, it keeps more
  garbage, in proportion to the players.
- The lobby shows the local machine and the first three remote machines.
  The other machines are also in the game.
- In free-for-all games, each player is a team.

Linux, Windows and Android machines can play in the same game. Each machine
simulates the players from the same inputs, and the host does not correct
all of the game. Thus each machine must calculate the same floating-point
results, and all the ports:

- Compile without fused multiply-add (`-ffp-contract=off`).
- Use the math functions of musl (`port/include/halo_math.h`,
  `port/third_party/musl-math`), not the math functions of the system.

### Play on one computer

More than one copy of the game can play on one computer. Each copy must
have a different loopback address. A copy with an address gets no
broadcasts. Thus each copy must send its broadcasts to the other copies.

For a host and two clients, enter these commands in three terminals:

```sh
HALO_NET_ADDRESS=127.0.0.200 HALO_NET_BROADCAST=127.0.0.201,127.0.0.202 build/linux/halo
HALO_NET_ADDRESS=127.0.0.201 HALO_NET_BROADCAST=127.0.0.200 build/linux/halo
HALO_NET_ADDRESS=127.0.0.202 HALO_NET_BROADCAST=127.0.0.200 build/linux/halo
```

Do not give 127.0.0.1 to a copy. Each copy gets to its own address through
127.0.0.1. Linux and Windows send all of 127.0.0.0/8 to the loopback
interface.

### Test with many machines

`tools/system_link_bots.py` adds simple machines to a game. Each machine has
one player. The machines obey the system link protocol, but they do not
calculate the game or move their players.

1. Start a game on the host.
2. Enter `python tools/system_link_bots.py --host 127.0.0.200 --machines 127 --start`.

Each machine uses its own loopback address, from 127.0.0.2. The option
`--start` starts the game when all the machines are in the lobby. If the
host has no `network.address`, do not give `--host`.

## Internet play

Machines with an invite link can play system link on the internet. This
project has no server.

When a copy of the game starts to host a system link game, it makes an
invite link: `halo://join/<64 hexadecimal digits>`. The game writes the link
to the standard error and puts it on the clipboard. The links of older
versions of the game (44 digits) do not operate. The game writes a message
when it gets one.

To join a game, do one of these steps:

- Open the link. The game is the handler of `halo://` links. If the game
  already operates, the new copy gives the link to it and stops. A key in a
  file that only the user can read (`halo-ce-universal.key` in
  `$XDG_RUNTIME_DIR`, else `~/.halo-ce-universal.key`; on Windows in
  `%LOCALAPPDATA%`) encrypts the link, so the programs of other users cannot
  read it.
- Copy the link (or the 64 digits) and go to the game.
- Enter `halo <link>`.
- Accept a Discord invite. Refer to "Discord".

When the machines connect, the game of the host shows in Multiplayer,
System Link. Join the game as on a local network. System link on a local
network does not need an invite.

### Security

Only machines with the invite can find the game:

- Each copy of the game makes an X25519 key pair when it starts. Its
  identifier is from the hash of its public key.
- The link contains a 16-byte hash of the public key of the host and a
  random 16-byte token. The identifier of the host is from the first 6
  bytes of the hash.
- The machines exchange their public keys and addresses through public MQTT
  brokers (`network.signalling_brokers`). The topics are HMACs of the token.
  A key from the token encrypts and authenticates the messages
  (`src/p2p_signal.c`, `src/p2p_crypto.c`). The host authenticates its answer
  with a key that only it and the player can calculate. Its public key must
  agree with the hash in the link. The hash is long, so no other machine can
  find a key with the same hash.
- Then the player shows in the same way that it has the private key of its
  public key. Only then does the host make a session for the player. Thus
  other machines with the invite cannot make sessions in the name of a
  player (such a session would keep the player out).
- Each two machines get the keys of their packets from their key pairs and
  a random number from each. The keys do not go through the brokers. Thus
  other machines with the invite cannot read or change the packets.
- Each packet is encrypted and authenticated, with a different key in each
  direction. A machine ignores a packet that it already received.
- A machine can send only to the ports of the game on the other machine.
- The host makes one session from each request of a player. If a person
  sends a copy of an old request again, the host ignores it. A player that
  must ask again sends a new request.
- The host tries to reach at most 8 new players at the same time. The
  other players ask again.
- The host answers a request that is not proven at most one time each
  second through each broker. It answers at most 20 of these requests each
  second, after a first 32. Each answer goes only through the broker that
  brought the request. Thus a flood of requests does not use much of the
  bandwidth of the host.
- The host does the key work of at most 20 requests each second from keys
  that it does not know, after a first 32. It keeps the key work of the
  last 256 keys. Thus the proof of a player does not need more key work. A
  flood of requests can make players join more slowly. A player asks again
  for 90 seconds.
- The host drops a player whose game runs faster than time (a speed hack)
  for ten seconds, and keeps that address out of its games. Each player
  sees who in red on the console. The host adds a line to `cheaters.txt`
  (beside `debug.txt`) with the address and hardware id of the player, and
  the Discord name and id that the game of the player told it (a player can
  change these). The host also bans the player: it adds the line to
  `bans.txt`, and refuses a machine whose address or hardware id is in it.
- The host can ban a player with `ban <player name>` in the developer
  console (Tab completes the name). Remove a line from `bans.txt` to unban.
  Refer to `NETCODE.md`. So that every player can be named, the host trims
  the spaces around a name and removes characters that draw as nothing. A
  letter with a mark is typed as the plain letter (`ban jose` for "José").
  A name with nothing left to type becomes "Player", and a name that another
  player already has gets a number ("Player 2"). The game refuses a profile
  name that is blank, and a multiplayer game refuses a profile whose name was
  made blank before this check.
- An invite operates while the copy of the game that made it operates.

### Connection

Each machine gets its public address from public STUN servers. Then the two
machines send packets to each other until the packets get through (UDP hole
punching). There is no relay.

Some networks give a different port for each destination (for example some
mobile and company networks). Two machines behind such networks cannot
connect. To connect, forward `network.tunnel_port` on the router of one of
the machines.

The game can ask the router to forward the port (UPnP,
`src/posix_upnp.c`, with `port/third_party/miniupnpc`):

- The host asks its router when a player uses its invite.
- A player that joins asks its router when it does not reach the host in
  5 seconds.
- The forwarded port is one more address that the machine gives to the
  other machine.
- The forward has a duration of one hour. The game makes it longer while
  it operates. When the game stops normally, it removes the forward. It
  does not remove the forward after a crash, or if a request to the router
  is still under way 3 seconds after the game starts to stop. Some routers only make forwards without a duration.
- When the game finds the router, it removes the forwards to this machine
  that have the description "Halo internet play" and that no copy of the
  game uses now (forwards that a copy of the game did not remove).
- UPnP does not help behind a second NAT, for example the NAT of a mobile
  network provider. Then the router has a private address, and the game
  does not ask.

To stop all UPnP requests, set `network.allow_upnp` to `false`.

In the game, each machine has an address in 100.64.0.0/10:

- `src/xnet.c` gives the datagrams that the bound UDP sockets of the game
  send to such an address to `src/p2p.c`. Other datagrams (of sockets that
  are not bound yet, or that are connected to the address) and the TCP
  connections of the game go through local sockets on 127.0.0.1 (or
  `network.address`). The traffic from the other machines comes to the
  game from local sockets too.
- `src/p2p.c` sends that traffic through one UDP socket. UDP datagrams go
  as they are. TCP connections go as KCP streams (`port/third_party/kcp`).
- The broadcasts of the game go to all the machines. Thus the game of the
  host shows on the other machines.

### Discord

If the Discord desktop client operates, the game of the host shows in
Discord (through the application of `discord.application_id`). The activity
has a private party with the invite as its join secret. The host can send
the invite with the invite button of Discord. When a person accepts it, that
person joins the game. If the game does not operate, Discord starts it.
The game sends the activity only to a Discord client of the same user.

## What operates

| Area | Status |
| --- | --- |
| Game code | All 466 C files of the game. The changes are in "Game source changes". |
| Graphics | Direct3D 8 on OpenGL 4.5 core through SDL3 (`src/d3d8_gl.c`). The port translates the NV2A vertex shaders and register combiners to GLSL. It decodes all the Xbox texture formats. The vertex and index buffers come from a GL copy of the Xbox memory. |
| High-res HUD | The HUD is drawn from high-res assets: redraws at 8x the size of the maps' bitmaps (4x for the largest), in `port/assets/hud`. They cover the meters, counters, panels and their outlines, the motion sensor, reticles, waypoints and scopes, but no bitmap with English text. `tools/hud_assets.py` makes them from the SVG redraws, and the build embeds them in the executable. When the game uploads one of those bitmaps, `src/hud_hires.c` gives the high-res texture in its place, if the bitmap's pixels are those of the English maps: another language's maps keep their own. The game sizes and places the HUD from its tags as before. `display.high_res_hud = false` turns this off. |
| High-res text | The menus' and HUD's text is drawn with Overpass (`port/assets/fonts`, SIL Open Font License) in place of the maps' bitmap fonts, which are Interstate. `src/text_hires.c` rasterizes each glyph with stb_truetype (`port/third_party/stb`) at the display's resolution, into an atlas that a placeholder bitmap of the game stands for. The game lays the text out from its font tags as before. The menus' titles (the screens' headers and the main menu's items) are pictures of text in the maps, so they are drawn as the high-res HUD is: `tools/title_assets.py` sets each one again in OpenCE, Roger White's public-domain Newtown respaced to match the maps' commercial title typeface (`tools/title_font.py`), at 4x the bitmap's size over its own plate or glow, each letter placed where the map's letter is, in `port/assets/titles`. The postgame carnage report's title is set over a hand-made SVG redraw of its panel (`port/assets/titles/svg`) instead. `display.high_res_text = false` turns it off. |
| Sound | Xbox DirectSound on SDL3 audio (`src/dsound_sdl.c`): PCM and Xbox ADPCM, mixed at 48 kHz, with volume, pitch, mix bins, distance, stereo pan, occlusion and obstruction. There is no Doppler effect, no cones and no reverb. |
| Input | XInput on SDL3 (`src/xinput_sdl.c`): keyboard, mouse, gamepads with rumble, and the debug keyboard for the console. |
| Files | The Win32 file functions and the MSVC file functions on POSIX, with the translation of Xbox paths. |
| Threads | Threads, events, mutexes, critical sections, interlocked operations and alertable waits. |
| Memory | The port reserves the Xbox memory at `0x80000000`. Thus the game gets the fixed addresses that it expects. |
| Saved games | The Xbox `UDATA` layout, with SHA-1 signatures. |
| Networking | Winsock on BSD sockets. System link on a local network and on the internet. |
| Bink video | Not available. The game skips the movies. |

## How the port operates

### The compiler

`tools/linux_build.py` compiles the game with clang and these options, which
give the ABI of the MSVC compiler:

- `--target=i686-linux-gnu`: 32-bit x86.
- `-fms-extensions`: the MSVC extensions.
- `-fshort-wchar`: 16-bit `wchar_t`.
- `-malign-double`: 8-byte alignment of 64-bit members.
- `-fcommon`: tentative definitions, as in C89.

glibc gives only ISO C (`__STRICT_ANSI__`). Thus POSIX names, for example
`random`, do not conflict with the names of the game.

These files supply the MSVC functions that clang does not have:

| File | Contents |
| --- | --- |
| `include/halo_linux_prefix.h` | The first header of each file: the architecture macros of the SDK, MSVC `__inline`, SEH keywords, `__declspec(selectany)`. |
| `include/` | Headers that add MSVC names to the C runtime headers. |
| `port/include/xdk` | The Xbox SDK declarations. The compiler reads this folder after all the other folders. |
| `tools/linux_msvc_semantics.py` | Makes a header that declares each struct tag at file scope, as MSVC does. It also makes the header inline functions weak, as the COMDAT functions of MSVC. `game/msvc_comdat.c` gives one external copy of each. |
| `include/halo_linux_winsock_names.h` | Gives new names to the Winsock functions of the SDK. Thus they do not link to the glibc functions with the same names. |
| `include/halo_linux_source_fixups.h` | Repairs one declaration conflict (`rasterizer_debug_drawing_begin`). |

`tools/linux_link_check.py` stops the link if a weak reference has no
definition. Without this check, the linker gives the reference the address
0.

### The platform layer (`src/`)

- The files `posix_*.c` use glibc. The compiler uses the ABI of the host
  for these files, because some glibc structures have a different layout
  with `-malign-double`.
- The other files include the SDK declarations through `platform.h`. Thus
  the compiler examines each definition against the SDK prototype.
- `src/halo_linker_common.c` gives weak storage for some globals of the
  January link, and for `fast_ftol_C` and `main_crash`.
- `main/d3d_intimacy.cpp` reads a private structure of the Xbox Direct3D.
  The Linux build does not use this file. `src/d3d8_gl.c` gives
  `d3d_find_flipcount`.
- The build returns small structures and unions in registers
  (`-freg-struct-return`), as on Win32.

### Game source changes

Five files of the game have changes for clang. These changes do not change
the MSVC objects: a comparison of all 612 C objects showed no difference in
code or data.

| File | Change |
| --- | --- |
| `cseries/cseries.c` | The naked function `stristr` uses `[ebp+8]` and `[ebp+12]` for its parameters. |
| `bitmaps/bitmap_drawing.c` | `*((word *)p)++` is now `*(*(word **)&p)++`. |
| `rasterizer/xbox/rasterizer_xbox_hardware_bitmaps.c` | `&(T *)x` is now `(T **)&x`. |
| `hs/hs.c` | Local prototypes that did not agree with `ai_script.h` are removed. |
| `units/vehicles.c` | The local prototype of `unit_update_animation` uses the type of `units.h`. |

`math/real_math.h` had a copy of `plane2d_from_points` that did not agree
with the function in `effects/decals.c`. clang used the copy, and parts of
levels were not visible. The copy now agrees with the function.

Other changes:

| File | Change |
| --- | --- |
| `scenario/scenario.c` | The BSP connection tables have names, not MSVC offsets. |
| `rasterizer/xbox/rasterizer_xbox_environment_fog.c` | A local pointer gets its value from the file-scope array with the same name. |
| `game/player_control.c` | The mouse aims the player on controller 1 directly. |
| `sound/game_sound.c` | The game calculates the obstruction of each sound one time for each tick, not for each frame. |
| `cseries/errors.c` | `debug.txt` stays open between lines. |
| `networking/`, `game/`, `interface/`, `bungie_net/network/` and the pools of objects, effects and sounds | The system link limits and the memory for them. |
| `game/`, `objects/`, `units/`, `networking/` | The distributed netcode. Refer to `NETCODE.md`. |
| `cache/cache_files.c` | When a map's tags load and unload, the port finds the bitmaps that the high-res HUD replaces (`game/hud_hires_tags.c`). |
| `interface/hud.c` | In multiplayer, players' names are drawn above their heads (`display.player_names`, `display.player_name_scale`). |
| `rasterizer/rasterizer_text.c`, `text/draw_string.c` | Text is drawn from an atlas of the fonts' glyphs, rasterized at the display's resolution (`src/text_hires.c`), when the font has every character of the string. Text can be drawn scaled about a point (`rasterizer_text_set_scale`), as the players' names are. Each glyph's advance is centred on the font tag character's, so the layout is the same, and a glyph is cut at a text box only where the font tag's character visibly was. |

The x86 inline assembly of the game is replaced by C. Thus the compiler
can optimize that code for each processor:

| File | Assembly | Replacement |
| --- | --- | --- |
| `cseries/cseries.h` | x87 `fistp` (`fast_ftol`) | `__builtin_rint` |
| `bitmaps/bitmaps_inlines.h` | x87 conversions | C conversions |
| `math/matrix_math.c` | SSE `matrix4x3_multiply` | a C loop |
| `effects/decals.c` | an x87 conversion | a C conversion |
| `cseries/profile.c` | `rdtsc` | `QueryPerformanceCounter` |
| `cseries/cseries.c` | naked `stristr` | a C `stristr` |
| `cseries/stack_walk_windows.c` | a read of EBP | `__builtin_frame_address` |
| `interface/hud_draw.c` | a read of `[ebp+4]` | `__builtin_return_address(1)` |
| `bink/bink_playback.c` | `int 3` | `__builtin_trap` |

The x87 control and status words (`_control87`, `_statusfp`, `_clearfp` in
`src/msvc_crt.c`) use `fenv.h`. On Android, they use the FPCR and FPSR.
