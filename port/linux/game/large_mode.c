/*
LARGE_MODE.C

The adapter between the game and the large-scale mode's Rust library
(rust/halo-client). The mode is
for about 500 players on a dedicated server (SpacetimeDB and a UDP gateway),
and is off unless large.map (HALO_LARGE_MAP) names a map. The existing mode
for up to 128 players is not touched.

A session starts from the settings, without the lobby:

- network_test.c hosts a one-machine game on large.map, the way
  debug.network_test does, so that the engine has a map, a local player and
  its unit (the engine will not start a game on one machine otherwise:
  network_server_manager.c asks large_mode_active(), as it asks the comparison
  harness's);
- when the game's map is loaded (large_mode_new_game) the library starts: it
  connects to SpacetimeDB directly and takes a seat in the match, and joins the
  gateway over UDP as the player the seat is;
- every tick, just before the objects are updated (large_mode_game_tick), the
  local player's unit is put where the server has it (once), and then moved by
  the library from the player's controls (see "the local player" below), whose
  new position and facing go to the gateway as that tick's input;
- when the game ends (large_mode_dispose) the library stops.

What the mode switches off: the distributed netcode's tick and message handling
(network_distributed.c, which a game of one machine would only idle through);
the C engine does not simulate other players, whom the library
holds from what the gateway sends. Each of them is drawn by the existing
renderer (see "remote players" below). The game logs what the library holds,
and where it has drawn each remote player and the local one, once a second,
for the automated test to compare with what the server sent.

The server owns spawns, deaths and score. The local player's unit is made
by the engine only while the server says the player is alive (the gate in
game_engine_should_spawn_player, large_mode_player_spawn), and is put where the
server spawned the player, facing as it says, whenever the server spawns the
player; when the server says the player is dead, or waiting for a respawn wave
because no starting location is free, the unit is killed and the HUD says how
long until the respawn or the wave (large_mode_state_message). Another
player the server says is dead is not drawn (the library leaves them out of
what it holds). The engine's own scoring is off in the mode: the scoreboard
(game_engine.c, large_mode_scoreboard_*) lists every player of the match from
the server's standing table, in range or not, and when the game has ended it
stays up with the winner (the next match follows, as it does a rotation).

The local player: the engine hands the player's unit the controls of the
tick (the throttle and where the player faces and aims) just before this
adapter runs. The library computes the movement from them and from the map's
collision data (the player's own copy of the map file, read on a thread of its
own at the start: halo_large_load_map), and the adapter puts the unit where it
says; the engine's physics is suspended for the unit, as it is for the
remote players' (it keeps choosing and playing animations). The library's
velocity goes into the unit after the objects are updated, which the suspended
physics zeroes, for what reads it (the motion sensor). Until the map is in the
engine moves the unit as it always did, and the library starts from wherever
the unit is when it can.

Remote players: the library holds the newest state of every other player the
gateway sends. For each the adapter makes a biped, in the team's colour, with
the engine's physics suspended (the engine keeps choosing and playing its
animations: each tick it is handed the controls of a player running at the
velocity the server gave, and then put where the server has it, facing as it
faces). The engine's player records (the names over heads, the teams, the
motion sensor's contacts) hold 128 players in all, so only that many of the
remote units have a player; the nearest ones do, and the rest are units alone,
drawn all the same. A player the library no longer holds in range (the gateway
has not sent them for a while, or they have left the match) has their unit
deleted; if the gateway sends them again, it is made afresh.

The library's interface (halo_large_*, rust/halo-client/src/ffi.rs) passes
only floats, 32-bit integers and pointers, and returns nothing by value but a
32-bit integer.

The player picks a server instead of a map when large.root names the root
database of a server list (and large.map is empty): the library lists the
servers (rust/halo-client browser.rs) and the player uses the developer
console (hs.c hands it the lines it does not know, as it does the host's ban
command), in the main menu or in a game:

  servers        the list: each server's map, game type and players, and who
                 you are (the identity a ban names)
  join <n|id>    take a seat on that server; once it has one, the game hosts a
                 game of one machine on the server's map, as above
  leave          stop being on the server

On a server the game follows its rotation: when the list names another match
for it, the game goes back to the lobby and joins that one. A server that
bans the player, or is full, says so on the console. The identity is kept in
the save root (u/large_identity), one file for each SpacetimeDB, and the name
the player chose (large.name) goes with it to every server's roster.

Fighting: the player's own engine fires the weapon and sees what it hits; it
reports a hit on another player to the server (large_mode_damage_deals, from
object_cause_damage: the engine deals no damage to a player's unit in this mode,
the server does), which checks the report and deals the damage. What the server
says of each player's health and shields (the library's halo_large_vitals) is
put into the engine's unit of the player every tick (large_mode_show_vitals), so
that the HUD's shield and health bars, and the flash of a shield that is hit and
the effects of one that goes down and comes back, are the engine's own. A player
the server says has been killed by a hit is killed in the engine too (the unit's
death plays, and its body stays as a body does). The local player holds the
weapon the server says they carry (large_mode_equip_local). With large.autofire
the local player aims at the nearest other player in the world and holds the
trigger, for the automated tests' shooter.

Without HALO_LARGE_MODE (the Android build, or a desktop build made without
the library) the mode is not there: large_mode_active() is FALSE.
*/

#include "cseries.h"
#include "game/game.h"
#include "game/game_engine.h"
#include "game/game_globals.h"
#include "game/players.h"
#include "items/weapons.h"
#include "items/weapon_definitions.h"
#include "objects/damage.h"
#include "objects/objects.h"
#include "scenario/scenario.h"
#include "tag_files/tag_groups.h"
#include "text/unicode.h"
#include "units/bipeds.h"
#include "units/unit_control_data.h"
#include "units/units.h"

#include <math.h>
#include <stdlib.h>
#include <string.h>

/* the platform layer's (port/linux/src/port_config.c, xbox_files.c) */
const char *config_string(char const *name);
long config_integer(char const *name);
int config_boolean(char const *name);
void platform_log(char const *format, ...);
void platform_translate_path(const char *xbox_path, char *host_path, unsigned long host_path_size);
char const *platform_save_root(void);

/* network_test.c's: the next game of a server the player picked */
void network_test_large_host(char const *map);
void network_test_large_leave_game(void);
void network_test_large_stop(void);

#ifdef HALO_LARGE_MODE

#include "main/console.h"

/* the library's (rust/halo-client/src/ffi.rs): `unsigned long` is 32 bits on
every build that has it */
typedef char large_mode_long_is_32_bits_assert[sizeof(unsigned long) == 4 ? 1 : -1];
unsigned long halo_large_start(const char *gateway, const char *spacetimedb, const char *database);
void halo_large_stop(void);
unsigned long halo_large_status(unsigned long *out);
unsigned long halo_large_frame(unsigned long *tick);
unsigned long halo_large_unit(unsigned long index, unsigned long *player, unsigned long *tick, float *out);
unsigned long halo_large_member(unsigned long player, unsigned long *team, char *name, unsigned long size);
unsigned long halo_large_local(float *out);
unsigned long halo_large_life(unsigned long *info, float *position);
unsigned long halo_large_game(unsigned long *out);
unsigned long halo_large_scoreboard_freeze(void);
unsigned long halo_large_scoreboard_row(unsigned long index, unsigned long *out, char *name, unsigned long size);
unsigned long halo_large_bounds(float *out);
void halo_large_send_input(float x, float y, float z, float yaw, float pitch);
unsigned long halo_large_load_map(const char *path);
void halo_large_place(float x, float y, float z);
unsigned long halo_large_move(float forward, float strafe, float yaw, float pitch, unsigned long jump,
	unsigned long crouch, float *out);
unsigned long halo_large_error(char *buffer, unsigned long size);
void halo_large_identity_dir(const char *folder);
void halo_large_set_name(const char *name);
unsigned long halo_large_browse_start(const char *spacetimedb, const char *database);
void halo_large_browse_stop(void);
unsigned long halo_large_browse_status(unsigned long *out);
unsigned long halo_large_browse_list(void);
unsigned long halo_large_browse_entry(unsigned long index, unsigned long *out);
unsigned long halo_large_browse_text(unsigned long index, unsigned long field, char *buffer, unsigned long size);
unsigned long halo_large_browse_find(const char *id);
unsigned long halo_large_browse_message(char *buffer, unsigned long size);
unsigned long halo_large_identity(char *buffer, unsigned long size);
unsigned long halo_large_refusal(char *buffer, unsigned long size);
unsigned long halo_large_vitals(unsigned long player, float *out);
unsigned long halo_large_loadout(unsigned long player, unsigned long *out);
unsigned long halo_large_weapon_name(unsigned long tag_index, char *buffer, unsigned long size);
unsigned long halo_large_report_hit(unsigned long target, unsigned long weapon, long material, float ox, float oy,
	float oz, float tx, float ty, float tz);
unsigned long halo_large_hits(unsigned long *out);

/* the local player's life, halo_large_life's first number */
enum
{
	_large_life_alive,
	_large_life_dead,
	_large_life_waiting
};

/* the server list's fields of halo_large_browse_text */
enum
{
	_large_text_id,
	_large_text_title,
	_large_text_map,
	_large_text_game_type,
	_large_text_variant,
	_large_text_database,
	_large_text_gateway
};

/* the numbers of halo_large_browse_entry, and the kinds halo_large_refusal returns */
enum
{
	_large_entry_players,
	_large_entry_capacity,
	_large_entry_match_number,
	_large_entry_seconds,
	_large_entry_joinable,
	k_large_entry_count
};

enum
{
	_large_refusal_none,
	_large_refusal_banned,
	_large_refusal_full,
	_large_refusal_other
};

/* where the player is, picking a server from the list */
enum
{
	_large_phase_idle,
	_large_phase_joining,
	_large_phase_playing,
	_large_phase_moving
};

/* seconds the player waits for a seat of a full server, or of one that does not answer */
#define LARGE_JOIN_PATIENCE 20.0f

static struct
{
	boolean checked;
	boolean active;
	char map[64];
	char gateway[128];
	char spacetimedb[256];
	char database[128];
	boolean log_players;

	/* the server list (large.root): the player picks the server, whose map,
	gateway and database then are the settings above; the server's id and title
	are for following its rotation and for the console */
	char root[128];
	boolean browser_mode;
	boolean browsing;
	long phase;
	real phase_seconds;
	boolean entered_game;
	char server_id[64];
	char server_title[96];
	char told_refusal[256];
	real followed_seconds;

	/* running: the library has been started for this game, the local player's
	unit has been put where the server has it, and the game tick the status was
	last logged on */
	boolean started;
	boolean placed;
	char logged_error[256];
	long logged_time;

	/* the local player (see large_mode_move_local): the unit the library moves,
	and with it the library's state of the last tick (position, velocity a
	second, whether it is in the air) */
	long local_suspended_unit;
	long local_seen_unit;
	boolean local_moving;
	boolean local_airborne;
	float local_state[7];
	/* (state[6] is how fast the tick drove the unit into the ground it landed on, a tick's worth) */
	long local_logged_time;

	/* the server's say of the local player's life: the spawn the unit was
	last put at (halo_large_life's third number), the unit that has been killed
	because the server says the player is dead, the life last logged, and
	large.scoreboard (the scoreboard stays up) */
	unsigned long spawn_seen;
	long killed_unit;
	long life_logged;
	boolean scoreboard_always;

	/* fighting: the player the server says the local player is, the unit that has been given the
	weapon the server says it carries, how many hits had hurt the local player when it was last
	logged, and large.autofire */
	unsigned long player_id;
	long equipped_unit;
	unsigned long hurts_logged;
	long equip_weapon;
	boolean vitals_short;
	long reports_logged;
	boolean autofire;
} large;

static void large_mode_read_settings(
	void)
{
	char const *map = config_string("large.map");

	large.checked = TRUE;
	if (!map[0])
	{
		/* no map: the player picks a server of the list, if there is one */
		snprintf(large.root, sizeof(large.root), "%s", config_string("large.root"));
		snprintf(large.gateway, sizeof(large.gateway), "%s", config_string("large.gateway"));
		snprintf(large.spacetimedb, sizeof(large.spacetimedb), "%s", config_string("large.spacetimedb"));
		large.log_players = config_boolean("large.log_players") != 0;
		large.scoreboard_always = config_boolean("large.scoreboard") != 0;
		large.autofire = config_boolean("large.autofire") != 0;
		if (large.root[0])
		{
			large.browser_mode = TRUE;
			platform_log("large mode: the server list of %s on %s (the console's servers, join and leave)", large.root,
				large.spacetimedb);
		}
		return;
	}
	snprintf(large.map, sizeof(large.map), "%s", map);
	snprintf(large.gateway, sizeof(large.gateway), "%s", config_string("large.gateway"));
	snprintf(large.spacetimedb, sizeof(large.spacetimedb), "%s", config_string("large.spacetimedb"));
	snprintf(large.database, sizeof(large.database), "%s", config_string("large.database"));
	large.log_players = config_boolean("large.log_players") != 0;
	large.scoreboard_always = config_boolean("large.scoreboard") != 0;
	large.autofire = config_boolean("large.autofire") != 0;
	if (!large.database[0])
	{
		platform_log("large mode: large.database names the match's database and cannot be missing: the game is "
			"played as usual");
		return;
	}
	large.active = TRUE;
	platform_log("large mode: %s, database %s, gateway %s, SpacetimeDB %s", large.map, large.database, large.gateway,
		large.spacetimedb);
}

boolean large_mode_active(
	void)
{
	if (!large.checked)
		large_mode_read_settings();
	return large.active;
}

char const *large_mode_map(
	void)
{
	return large_mode_active() ? large.map : "";
}

/* the library's latest error, each time it is a new one */
static void large_mode_log_error(
	void)
{
	char message[sizeof(large.logged_error)];

	if (halo_large_error(message, sizeof(message)) > 0 && strcmp(message, large.logged_error))
	{
		strcpy(large.logged_error, message);
		platform_log("large mode: %s", message);
	}
}

static void large_mode_forget_remotes(void);
static void large_mode_log_remotes(void);
static void large_mode_local_after_objects(void);
static void large_mode_show_vitals(long unit_index, unsigned long player);

/* the player's own copy of the match's map, for the local player's movement
(the library reads it on a thread of its own; call after a session has started,
which ends the library's hold of an earlier map) */
static void large_mode_load_map(
	void)
{
	char xbox_path[128];
	char path[1024];

	snprintf(xbox_path, sizeof(xbox_path), "d:\\maps\\%s.map", large.map);
	platform_translate_path(xbox_path, path, sizeof(path));
	halo_large_load_map(path);
}

/* a game's map is loaded: connect (before anything of the map makes an
object: nothing of the mode needs any) */
void large_mode_new_game(
	void)
{
	if (!large_mode_active())
		return;
	large.placed = FALSE;
	large.logged_error[0] = 0;
	large.logged_time = 0;
	large.local_suspended_unit = NONE;
	large.local_seen_unit = NONE;
	large.local_moving = FALSE;
	large.spawn_seen = 0xFFFFFFFFUL;
	large.killed_unit = NONE;
	large.life_logged = NONE;
	large.equipped_unit = NONE;
	large.equip_weapon = NONE;
	large.hurts_logged = 0;
	large.vitals_short = FALSE;
	large.reports_logged = 0;
	large_mode_forget_remotes();
	/* (with a server list the session is the player's join's, which started it) */
	if (large.browser_mode)
		return;
	large.started = halo_large_start(large.gateway, large.spacetimedb, large.database) != 0;
	if (!large.started)
		large_mode_log_error();
	else
		large_mode_load_map();
}

/* the game is over, or another map is loading */
void large_mode_dispose(
	void)
{
	if (!large.started || large.browser_mode)
		return;
	halo_large_stop();
	large.started = FALSE;
	large.placed = FALSE;
	large.local_suspended_unit = NONE;
	large.local_seen_unit = NONE;
	large.local_moving = FALSE;
	large_mode_forget_remotes();
	platform_log("large mode: stopped");
}

/* the local player's unit, or NULL while it is not in play */
static struct unit_datum *large_mode_local_unit(
	long *unit_index)
{
	long player_index = local_player_get_player_index(0);
	struct player_datum *player;

	if (player_index == NONE)
		return NULL;
	player = player_get(player_index);
	if (player->unit_index == NONE)
		return NULL;
	*unit_index = player->unit_index;
	return (struct unit_datum *)object_try_and_get_and_verify_type(player->unit_index, _object_mask_biped);
}

/* ---------- remote players */

enum
{
	/* the module's player ids (a match holds 500 unless its operator says
	otherwise) */
	LARGE_MAXIMUM_REMOTES = 512,
	/* how often the engine's players are shared out again among the units, and
	how many are swapped at most each time */
	LARGE_REBALANCE_TICKS = 30,
	LARGE_REBALANCE_SWAPS = 4,
	LARGE_NAME_LENGTH = 11,
};

/* the system's (milliseconds: the adapter's cost is told in cycles, which this clock gives a rate) */
unsigned long system_milliseconds(void);

/* players.c's */
void placement_data_set_change_color(struct object_placement_data *placement_data, real_rgb_color const *change_color);
void network_player_attach_unit(long player_index, long unit_index);
void network_player_detach_unit(long player_index);

struct large_remote
{
	boolean present;
	/* the unit, and the engine player that has it (NONE: it has none) */
	long unit_index;
	long player_index;
	long team;
	char name[LARGE_NAME_LENGTH + 1];
	/* the state the unit is drawn from: the library's, as of the datagram of
	this tick; the ninth number is the player's flags (1 in the air, 2 crouched) */
	unsigned long tick;
	float state[9];
	/* what the engine's animation needs of it, that its suspended physics does
	not give (large_mode_biped_state): in the air, and the speed at which it landed
	this tick, a tick's worth, when it did */
	boolean airborne;
	real landing_velocity;
	float last_fall_speed;
	/* since it was last logged: whether the engine's unit had a landing (soft or hard) to
	recover from, which is short and so seen between the log's seconds */
	boolean saw_landing;
};

static struct
{
	struct large_remote remotes[LARGE_MAXIMUM_REMOTES];
	/* the engine's players that are remote ones, by their slot */
	boolean player_is_remote[HALO_PORT_MAXIMUM_NETWORK_PLAYERS];
	long count;
	long players;
	long created;
	long removed;
	long create_failures;
	/* landings the library's states gave the engine (large_mode_biped_state) */
	long landings;
	long shared_time;
	long logged_time;
	/* what the adapter costs: the processor's cycles of this tick's update and
	of its tail, and over the window since the last log their sum, the worst
	tick, the ticks, and when the window began (in cycles and milliseconds) */
	unsigned long long tick_cycles;
	unsigned long long window_cycles;
	unsigned long long worst_cycles;
	unsigned long window_ticks;
	unsigned long long window_start_cycles;
	unsigned long window_start_ms;
	boolean ignored_ids_said;
	/* the remote unit of each engine object, for large_mode_biped_state (the
	remote's slot, one more than it: 0 is none) */
	short remote_of_object[HALO_PORT_MAXIMUM_OBJECTS_PER_MAP];
} large_remote_data;

/* whether the engine's player is a remote one (players.c leaves its controls
to the adapter) */
boolean large_mode_remote_player(
	long player_index)
{
	long slot = DATUM_INDEX_TO_ABSOLUTE_INDEX(player_index);

	return large_remote_data.count > 0 && slot >= 0 && slot < HALO_PORT_MAXIMUM_NETWORK_PLAYERS &&
		large_remote_data.player_is_remote[slot];
}

/* a remote unit's engine player: the name, the team, and the unit its own */
static boolean large_mode_give_player(
	struct large_remote *remote)
{
	struct network_player network_player;
	struct player_datum *player;
	long player_index;
	long index;

	csmemset(&network_player, 0, sizeof(network_player));
	for (index = 0; index < LARGE_NAME_LENGTH && remote->name[index]; index++)
		network_player.name[index] = (wchar_t)((byte)remote->name[index] < 0x80 ? remote->name[index] : '?');
	network_player.machine_index = (char)NONE;
	network_player.controller_index = (char)NONE;
	network_player.team_index = (char)remote->team;
	network_player.player_list_index = (char)NONE;
	player_index = player_new(NONE, NONE, NONE, &network_player);
	if (player_index == NONE)
		return FALSE;
	player = player_get(player_index);
	/* (what game_engine_player_added does for a player, but not its choice of team) */
	player->state_message = NONE;
	player->state_message_player_index = NONE;
	player->teleporter_index = NONE;
	player->player_display_index = NONE;
	player->team_index = remote->team;
	network_player_attach_unit(player_index, remote->unit_index);
	large_remote_data.player_is_remote[DATUM_INDEX_TO_ABSOLUTE_INDEX(player_index)] = TRUE;
	remote->player_index = player_index;
	large_remote_data.players++;
	return TRUE;
}

/* ... and takes it away again, leaving the unit */
static void large_mode_take_player(
	struct large_remote *remote)
{
	if (remote->player_index == NONE)
		return;
	network_player_detach_unit(remote->player_index);
	large_remote_data.player_is_remote[DATUM_INDEX_TO_ABSOLUTE_INDEX(remote->player_index)] = FALSE;
	/* (a player whose name is up over the one that is gone, as the player in the crosshair's is, shows nobody) */
	{
		struct data_iterator iterator;
		struct player_datum *other;

		data_iterator_new(&iterator, player_data);
		while ((other = (struct player_datum *)data_iterator_next(&iterator)) != NULL)
		{
			if (other->player_display_index == remote->player_index)
			{
				other->player_display_index = NONE;
				other->player_display_count = 0;
			}
		}
	}
	datum_delete(player_data, remote->player_index);
	remote->player_index = NONE;
	large_remote_data.players--;
	/* (no player's unit now, but still driven) */
	if (object_try_and_get_and_verify_type(remote->unit_index, _object_mask_unit))
		unit_set_actively_controlled(remote->unit_index, TRUE);
}

/* a player out of range, or gone from the match: the unit goes, and the engine's player with it */
static void large_mode_remove_remote(
	unsigned long id)
{
	struct large_remote *remote = &large_remote_data.remotes[id];

	large_mode_take_player(remote);
	large_remote_data.remote_of_object[DATUM_INDEX_TO_ABSOLUTE_INDEX(remote->unit_index)] = 0;
	if (object_try_and_get_and_verify_type(remote->unit_index, _object_mask_unit))
		object_delete(remote->unit_index);
	remote->present = FALSE;
	large_remote_data.count--;
	large_remote_data.removed++;
	if (large.log_players)
		platform_log("large mode: player %lu is out of range: unit deleted", id);
}

/* a player the server says a hit has killed: the engine's unit dies as a unit does (its physics, which the
adapter suspended, goes on from where it is) and stays as a body, which the engine takes away in time; the
adapter forgets it, as it does one that is out of range, and the player is made afresh when they spawn */
static void large_mode_kill_remote(
	unsigned long id)
{
	struct large_remote *remote = &large_remote_data.remotes[id];

	large_mode_take_player(remote);
	large_remote_data.remote_of_object[DATUM_INDEX_TO_ABSOLUTE_INDEX(remote->unit_index)] = 0;
	if (object_try_and_get_and_verify_type(remote->unit_index, _object_mask_unit))
	{
		unit_scripting_suspended(remote->unit_index, FALSE);
		unit_kill(remote->unit_index);
	}
	remote->present = FALSE;
	large_remote_data.count--;
	large_remote_data.removed++;
	platform_log("large mode: player %lu was killed: its unit dies", id);
}

/* a player who is no longer in the world: a body if the server's say of their health is that a hit
killed them, or else they are out of range or gone, and their unit goes */
static void large_mode_lose_remote(
	unsigned long id)
{
	float vitals[6];

	if (halo_large_vitals(id, vitals) && ((long)(vitals[3] + 0.5f) & 2) != 0)
		large_mode_kill_remote(id);
	else
		large_mode_remove_remote(id);
}

/* a unit for a player the gateway has sent, where it is, once the match's
roster says who the player is */
static boolean large_mode_create_remote(
	unsigned long id,
	unsigned long tick,
	float const *state)
{
	struct large_remote *remote = &large_remote_data.remotes[id];
	struct game_globals *globals = scenario_get_game_globals();
	struct game_globals_multiplayer_information *information;
	struct object_placement_data placement;
	real_rgb_color color;
	unsigned long team;
	long unit_index;
	struct unit_datum *unit;

	if (!halo_large_member(id, &team, remote->name, sizeof(remote->name)))
		return FALSE;
	information = TAG_BLOCK_GET_ELEMENT(&globals->multiplayer_information, 0,
		struct game_globals_multiplayer_information);
	if (information->unit.index == NONE)
		return FALSE;
	object_placement_data_new(&placement, information->unit.index, NONE);
	placement.position.x = state[0];
	placement.position.y = state[1];
	placement.position.z = state[2];
	placement.forward.i = (real)cos(state[6]);
	placement.forward.j = (real)sin(state[6]);
	placement.forward.k = 0.0f;
	placement.up = *global_up3d;
	placement.owner_team_index = (short)team;
	/* (the engine's own colours: red is team 0 and blue the other) */
	color = team == 0 ? *global_real_rgb_red : *global_real_rgb_blue;
	placement_data_set_change_color(&placement, &color);
	unit_index = object_new(&placement);
	unit = (struct unit_datum *)object_try_and_get_and_verify_type(unit_index, _object_mask_unit);
	if (!unit)
	{
		large_remote_data.create_failures++;
		return FALSE;
	}
	unit->object.owner_team_index = (short)team;
	/* the engine moves it no more than the library does: its physics keeps the
	position it is given, and its animation goes on */
	unit_scripting_suspended(unit_index, TRUE);
	unit_set_actively_controlled(unit_index, TRUE);
	remote->present = TRUE;
	remote->unit_index = unit_index;
	remote->airborne = FALSE;
	remote->landing_velocity = 0.0f;
	remote->last_fall_speed = 0.0f;
	large_remote_data.remote_of_object[DATUM_INDEX_TO_ABSOLUTE_INDEX(unit_index)] = (short)(id + 1);
	remote->player_index = NONE;
	remote->team = (long)team;
	remote->tick = tick;
	memcpy(remote->state, state, sizeof(remote->state));
	large_remote_data.count++;
	large_remote_data.created++;
	/* (a unit with no player, if the engine has none to give, until one is swapped to it) */
	large_mode_give_player(remote);
	if (large.log_players)
		platform_log("large mode: player %lu appears: \"%s\" team %lu", id, remote->name, team);
	return TRUE;
}

/* the engine's players go to the units nearest the local player (names and the
motion sensor matter near), the first come where there is room, and a few swap
as the players move */
static void large_mode_share_players(
	void)
{
	long swaps = 0;
	long id;
	long unit_index;
	struct unit_datum *local = large_mode_local_unit(&unit_index);
	real_point3d origin = { { 0.0f, 0.0f, 0.0f } };

	if (local)
		origin = local->object.position;

	for (;;)
	{
		struct large_remote *nearest = NULL, *farthest = NULL;
		real nearest_distance = 0.0f, farthest_distance = 0.0f;

		for (id = 0; id < LARGE_MAXIMUM_REMOTES; id++)
		{
			struct large_remote *remote = &large_remote_data.remotes[id];
			real distance;

			if (!remote->present)
				continue;
			distance = (remote->state[0] - origin.x) * (remote->state[0] - origin.x) +
				(remote->state[1] - origin.y) * (remote->state[1] - origin.y) +
				(remote->state[2] - origin.z) * (remote->state[2] - origin.z);
			if (remote->player_index == NONE)
			{
				if (!nearest || distance < nearest_distance)
				{
					nearest = remote;
					nearest_distance = distance;
				}
			}
			else if (!farthest || distance > farthest_distance)
			{
				farthest = remote;
				farthest_distance = distance;
			}
		}
		if (!nearest)
			break;
		if (large_mode_give_player(nearest))
			continue;
		/* no room: the farthest has to be over half as far again as the nearest (the distances
		are squared) to be swapped for it */
		if (!farthest || swaps >= LARGE_REBALANCE_SWAPS || nearest_distance * 2.0f > farthest_distance)
			break;
		large_mode_take_player(farthest);
		if (!large_mode_give_player(nearest))
			break;
		swaps++;
	}
}

/* what an engine unit is given of the library's state of a player: the controls
of a player running at that velocity, facing as the player does; then it is
put where the player is */
static void large_mode_drive_remote(
	struct large_remote *remote,
	struct game_globals_player_information const *information)
{
	float const *state = remote->state;
	struct unit_control_data control;
	real_point3d position;
	real_vector3d up;
	real cos_yaw = (real)cos(state[6]);
	real sin_yaw = (real)sin(state[6]);
	real cos_pitch = (real)cos(state[7]);
	real ahead = state[3] * cos_yaw + state[4] * sin_yaw;
	real left = -state[3] * sin_yaw + state[4] * cos_yaw;
	long flags = (long)(state[8] + 0.5f);
	boolean airborne = (flags & 1) != 0;
	boolean crouched = (flags & 2) != 0;

	csmemset(&control, 0, sizeof(control));
	control.animation_state = _unit_animation_state_in_combat;
	control.weapon_index = NONE;
	control.grenade_index = NONE;
	control.zoom_level = NONE;
	/* crouched, the player is at the sneaking speeds, and the engine's animation sinks the
	unit into the crouch (which it does for the control flag) */
	if (crouched)
		SET_FLAG(control.control_flags, _unit_control_crouch_modifier_bit, TRUE);
	/* (the tags' speeds are never zero, but a throttle must never be not a number) */
	control.throttle.i = PIN(ahead / MAX(ahead > 0.0f ? (crouched ? information->sneak_forward_speed :
		information->run_forward_speed) : (crouched ? information->sneak_backward_speed :
		information->run_backward_speed), 0.001f), -1.0f, 1.0f);
	control.throttle.j = PIN(left / MAX(crouched ? information->sneak_sideways_speed : information->run_sideways_speed,
		0.001f), -1.0f, 1.0f);
	/* a player who was in the air and is not has landed, at about the speed they were falling: the engine
	starts the landing, soft or hard, from it (the fall is the faster of the two states' speeds, the
	later state's being the one that has the fall's last tick in it) */
	remote->landing_velocity = 0.0f;
	if (remote->airborne && !airborne)
		remote->landing_velocity = MAX(-MIN(remote->last_fall_speed, state[5]), 0.0f) / TICKS_PER_SECOND;
	remote->airborne = airborne;
	remote->last_fall_speed = state[5];
	control.facing_vector.i = cos_yaw;
	control.facing_vector.j = sin_yaw;
	control.facing_vector.k = 0.0f;
	control.aiming_vector.i = cos_yaw * cos_pitch;
	control.aiming_vector.j = sin_yaw * cos_pitch;
	control.aiming_vector.k = (real)sin(state[7]);
	control.looking_vector = control.aiming_vector;
	unit_control(remote->unit_index, &control);

	position.x = state[0];
	position.y = state[1];
	position.z = state[2];
	up = *global_up3d;
	object_set_position(remote->unit_index, &position, &control.facing_vector, &up);
}

/* the remote players, from what the library holds, before the objects are
updated: a unit for each the gateway sends, driven; none for those out of range */
static void large_mode_update_remotes_work(
	void)
{
	static boolean seen[LARGE_MAXIMUM_REMOTES];
	struct game_globals_player_information const *information;
	unsigned long frame_tick;
	/* (the players in range) */
	unsigned long count = halo_large_frame(&frame_tick);
	unsigned long index;
	long id;

	csmemset(seen, 0, sizeof(seen));
	for (index = 0; index < count; index++)
	{
		unsigned long player, tick;
		float state[9];

		if (!halo_large_unit(index, &player, &tick, state))
			continue;
		if (player >= LARGE_MAXIMUM_REMOTES)
		{
			if (!large_remote_data.ignored_ids_said)
			{
				large_remote_data.ignored_ids_said = TRUE;
				platform_log("large mode: player %lu is past the %d the client holds: not drawn", player,
					LARGE_MAXIMUM_REMOTES);
			}
			continue;
		}
		if (!large_remote_data.remotes[player].present && !large_mode_create_remote(player, tick, state))
			continue;
		seen[player] = TRUE;
		large_remote_data.remotes[player].tick = tick;
		memcpy(large_remote_data.remotes[player].state, state, sizeof(large_remote_data.remotes[player].state));
	}

	for (id = 0; id < LARGE_MAXIMUM_REMOTES; id++)
	{
		if (large_remote_data.remotes[id].present && !seen[id])
			large_mode_lose_remote((unsigned long)id);
	}
	if (large_remote_data.count <= 0)
		return;

	if (game_time_get() - large_remote_data.shared_time >= LARGE_REBALANCE_TICKS)
	{
		large_remote_data.shared_time = game_time_get();
		large_mode_share_players();
	}

	information = TAG_BLOCK_GET_ELEMENT(&scenario_get_game_globals()->player_information, 0,
		struct game_globals_player_information);
	for (id = 0; id < LARGE_MAXIMUM_REMOTES; id++)
	{
		if (large_remote_data.remotes[id].present)
		{
			large_mode_drive_remote(&large_remote_data.remotes[id], information);
			large_mode_show_vitals(large_remote_data.remotes[id].unit_index, (unsigned long)id);
		}
	}
}

static void large_mode_update_remotes(
	void)
{
	unsigned long long start = __builtin_ia32_rdtsc();

	large_mode_update_remotes_work();
	large_remote_data.tick_cycles = __builtin_ia32_rdtsc() - start;
}

/* called from game_tick, after the objects are updated: the engine has no
velocity for the units (the suspended physics zeroes it), and the motion
sensor, which shows what moves, reads it: the library's */
void large_mode_game_tick_after_objects(
	void)
{
	unsigned long long start = __builtin_ia32_rdtsc();
	long id;

	if (!large.started)
		return;
	large_mode_local_after_objects();
	if (large_remote_data.count <= 0)
		return;
	for (id = 0; id < LARGE_MAXIMUM_REMOTES; id++)
	{
		struct large_remote *remote = &large_remote_data.remotes[id];
		struct object_datum *object;

		if (!remote->present)
			continue;
		object = (struct object_datum *)object_try_and_get_and_verify_type(remote->unit_index, _object_mask_unit);
		if (!object)
			continue;
		/* (the engine's velocities are a tick's worth) */
		object->object.translational_velocity.i = remote->state[3] / TICKS_PER_SECOND;
		object->object.translational_velocity.j = remote->state[4] / TICKS_PER_SECOND;
		object->object.translational_velocity.k = remote->state[5] / TICKS_PER_SECOND;
		if (((struct biped_datum *)object)->biped.landing != NONE)
			remote->saw_landing = TRUE;
	}

	large_remote_data.tick_cycles += __builtin_ia32_rdtsc() - start;
	large_remote_data.window_cycles += large_remote_data.tick_cycles;
	large_remote_data.worst_cycles = MAX(large_remote_data.worst_cycles, large_remote_data.tick_cycles);
	large_remote_data.window_ticks++;

	if (game_time_get() - large_remote_data.logged_time >= TICKS_PER_SECOND)
	{
		large_remote_data.logged_time = game_time_get();
		large_mode_log_remotes();
	}
}

/* once a second, after the objects are updated: how many remote units there
are, and with large.log_players where the engine has each, with the tick of the
state it was driven from (the automated test compares these with the server's) */
static void large_mode_log_remotes(
	void)
{
	unsigned long long now_cycles = __builtin_ia32_rdtsc();
	unsigned long now_ms = system_milliseconds();
	long id;

	platform_log("large mode: %ld remote units, %ld with players | created %ld removed %ld failures %ld landings %ld",
		large_remote_data.count, large_remote_data.players, large_remote_data.created, large_remote_data.removed,
		large_remote_data.create_failures, large_remote_data.landings);
	/* what the adapter cost a tick over the last second: the window's cycles at
	the rate the window's own length gives them */
	if (large_remote_data.window_start_ms && now_ms > large_remote_data.window_start_ms && large_remote_data.window_ticks)
	{
		double cycles_per_ms = (double)(now_cycles - large_remote_data.window_start_cycles) /
			(double)(now_ms - large_remote_data.window_start_ms);

		platform_log("large mode: the adapter cost %.3f ms a tick over %lu ticks, %.3f ms at worst",
			(double)large_remote_data.window_cycles / cycles_per_ms / (double)large_remote_data.window_ticks,
			large_remote_data.window_ticks, (double)large_remote_data.worst_cycles / cycles_per_ms);
	}
	large_remote_data.window_start_cycles = now_cycles;
	large_remote_data.window_start_ms = now_ms;
	large_remote_data.window_cycles = 0;
	large_remote_data.worst_cycles = 0;
	large_remote_data.window_ticks = 0;
	if (!large.log_players)
		return;
	for (id = 0; id < LARGE_MAXIMUM_REMOTES; id++)
	{
		struct large_remote *remote = &large_remote_data.remotes[id];
		struct object_datum *object;

		if (!remote->present)
			continue;
		object = (struct object_datum *)object_try_and_get_and_verify_type(remote->unit_index, _object_mask_unit);
		if (!object)
			continue;
		/* (and what the engine's animation made of the player's flags: its state, which the
		airborne and the landing ones are among, and its base seat, which the crouch is) */
		platform_log("large mode: drawn %ld tick %lu (%.4f %.4f %.4f) team %ld player %ld flags %ld state %ld "
			"seat %ld landing %ld", id, remote->tick,
			object->object.position.x, object->object.position.y, object->object.position.z, remote->team,
			remote->player_index == NONE ? -1L : (long)DATUM_INDEX_TO_ABSOLUTE_INDEX(remote->player_index),
			(long)(remote->state[8] + 0.5f), (long)((struct unit_datum *)object)->unit.animation.state,
			(long)((struct unit_datum *)object)->unit.animation.base_seat_index,
			remote->saw_landing ? 1L : (long)((struct biped_datum *)object)->biped.landing);
		remote->saw_landing = FALSE;
	}
}

/* a game starts or ends: the engine has deleted the objects and the players with the map */
static void large_mode_forget_remotes(
	void)
{
	csmemset(&large_remote_data, 0, sizeof(large_remote_data));
}

/* once a second: where the session stands, and with large.log_players every
player the gateway has sent, as the library holds them (the automated test
compares these with what the server sent) */
static void large_mode_log(
	void)
{
	unsigned long status[8];
	boolean joined;
	unsigned long tick;
	unsigned long count;
	unsigned long index;

	joined = halo_large_status(status) != 0;
	count = halo_large_frame(&tick);
	platform_log("large mode: tick %ld joined %d slow %lu map %lu player %lu | joins sent %lu datagrams %lu bytes %lu "
		"undecoded %lu inputs %lu | gateway tick %lu, %lu players",
		game_time_get(), joined ? 1 : 0, status[0], status[1], status[6], status[2], status[3], status[4], status[5],
		status[7], tick, count);
	if (large.log_players)
	{
		long unit_index;
		struct unit_datum *unit = large_mode_local_unit(&unit_index);
		short slot;

		for (slot = 0; unit && slot < MAXIMUM_WEAPONS_PER_UNIT; slot++)
		{
			struct weapon_datum *weapon = unit->unit.weapon_object_indices[slot] != NONE ?
				weapon_try_and_get(unit->unit.weapon_object_indices[slot]) : NULL;

			if (weapon)
			{
				struct weapon_magazine const *magazine = &weapon->weapon.magazines[0];

				platform_log("large mode: the local unit has %s in slot %d%s (rounds %d of %d, heat %.2f, state %d)",
					tag_get_name(weapon->definition_index), slot, slot == unit->unit.current_weapon_index ? ", in hand" : "",
					magazine->rounds_loaded, magazine->rounds_total, weapon->weapon.heat, weapon->weapon.state);
			}
		}
	}
	{
		unsigned long hits[2];
		float vitals[6];

		if (halo_large_hits(hits) && hits[0])
			platform_log("large mode: %lu hits reported in %lu calls", hits[0], hits[1]);
		/* the local player's health and shields, while a hit has left them short, and once when they are
		whole again */
		if (halo_large_vitals(status[6], vitals))
		{
			boolean whole = vitals[0] >= 1.0f && vitals[1] >= 1.0f;

			if (!whole || large.vitals_short || (unsigned long)vitals[4] != large.hurts_logged)
			{
				large.hurts_logged = (unsigned long)vitals[4];
				large.vitals_short = !whole;
				platform_log("large mode: the local player's shield is %.3f and health %.3f (%lu hits have hurt them, "
					"last by player %ld)", vitals[0], vitals[1], large.hurts_logged, (long)vitals[5]);
			}
		}
	}
	if (!large.log_players)
		return;
	for (index = 0; index < count; index++)
	{
		unsigned long player, unit_tick;
		float state[9];

		if (halo_large_unit(index, &player, &unit_tick, state))
		{
			platform_log("large mode: player %lu tick %lu (%.4f %.4f %.4f) v (%.3f %.3f %.3f) yaw %.4f pitch %.4f "
				"flags %.0f", player, unit_tick, state[0], state[1], state[2], state[3], state[4], state[5], state[6],
				state[7], state[8]);
		}
	}
}

/* ---------- the local player */

/* the local player's movement for this tick. The unit has this tick's controls
(the player's action was handed to it just before: its throttle, and the aiming
the facing follows); the library moves the player by them through the map's
collision data, and the unit goes where it says, with the engine's own physics
suspended for it so that it does not move it again. The library sends the new
position to the gateway as the tick's input. When the library cannot yet (the
map is still being read) the engine moves the unit, the library follows it, and
its position goes to the gateway instead. */
static void large_mode_move_local(
	long unit_index,
	struct unit_datum *unit)
{
	real_vector3d const *aim = &unit->unit.desired_aiming_vector;
	float yaw = (float)atan2(aim->j, aim->i);
	float pitch = (float)asin(PIN(aim->k, -1.0f, 1.0f));
	real_point3d position;
	unsigned long moved;

	/* a unit that is new (the player's first, or the next after a death) is where
	the engine has put it: the library starts from there */
	if (large.local_seen_unit != unit_index)
	{
		large.local_seen_unit = unit_index;
		halo_large_place(unit->object.position.x, unit->object.position.y, unit->object.position.z);
	}
	moved = halo_large_move(unit->unit.throttle.i, unit->unit.throttle.j, yaw, pitch,
		TEST_FLAG(unit->unit.control_flags, _unit_control_jump_bit) ? 1 : 0,
		TEST_FLAG(unit->unit.control_flags, _unit_control_crouch_modifier_bit) ? 1 : 0, large.local_state);

	if (!moved)
	{
		halo_large_place(unit->object.position.x, unit->object.position.y, unit->object.position.z);
		halo_large_send_input(unit->object.position.x, unit->object.position.y, unit->object.position.z, yaw, pitch);
		large.local_moving = FALSE;
		return;
	}

	/* (a new unit, after a respawn, is the library's to move from its first tick) */
	if (large.local_suspended_unit != unit_index)
	{
		unit_scripting_suspended(unit_index, TRUE);
		large.local_suspended_unit = unit_index;
		platform_log("large mode: the library moves the local unit from now on");
	}
	position.x = large.local_state[0];
	position.y = large.local_state[1];
	position.z = large.local_state[2];
	object_translate(unit_index, &position, NULL);
	large.local_airborne = (moved & 2) != 0;
	large.local_moving = TRUE;
}

/* whether the engine's physics, suspended for a unit, is to take the unit for one the library
moves, and what the library says of it: in the air (the jump and fall animations) and how fast it
landed this tick (the landing's). Called by the engine's biped update, for each unit suspended. The
engine does not hurt a unit for the fall: the server does. */
boolean large_mode_biped_state(
	long biped_index,
	boolean *airborne,
	real *landing_velocity)
{
	long slot;

	if (!large.started)
		return FALSE;
	if (large.local_moving && biped_index == large.local_suspended_unit)
	{
		*airborne = large.local_airborne;
		*landing_velocity = large.local_state[6];
		return TRUE;
	}
	if (large_remote_data.count <= 0)
		return FALSE;
	slot = DATUM_INDEX_TO_ABSOLUTE_INDEX(biped_index);
	if (slot >= 0 && slot < HALO_PORT_MAXIMUM_OBJECTS_PER_MAP && large_remote_data.remote_of_object[slot] > 0)
	{
		struct large_remote *remote = &large_remote_data.remotes[large_remote_data.remote_of_object[slot] - 1];

		if (remote->present && remote->unit_index == biped_index)
		{
			*airborne = remote->airborne;
			*landing_velocity = remote->landing_velocity;
			if (remote->landing_velocity > 0.0f)
				large_remote_data.landings++;
			return TRUE;
		}
	}
	return FALSE;
}

/* ... and after the objects are updated, its velocity: the engine has none for
a unit whose physics is suspended, and the motion sensor reads it */
static void large_mode_local_after_objects(void)
{
	struct object_datum *object;

	if (!large.local_moving)
		return;
	object = (struct object_datum *)object_try_and_get_and_verify_type(large.local_suspended_unit, _object_mask_unit);
	if (!object)
		return;
	/* (the engine's velocities are a tick's worth) */
	object->object.translational_velocity.i = large.local_state[3] / TICKS_PER_SECOND;
	object->object.translational_velocity.j = large.local_state[4] / TICKS_PER_SECOND;
	object->object.translational_velocity.k = large.local_state[5] / TICKS_PER_SECOND;

	if (game_time_get() - large.local_logged_time >= TICKS_PER_SECOND)
	{
		large.local_logged_time = game_time_get();
		/* (the automated tests compare where the engine has the unit with where the library has the player) */
		platform_log("large mode: local unit (%.4f %.4f %.4f) library (%.4f %.4f %.4f) v (%.3f %.3f %.3f) throttle "
			"(%.2f %.2f) airborne %d",
			object->object.position.x, object->object.position.y, object->object.position.z, large.local_state[0],
			large.local_state[1], large.local_state[2], large.local_state[3], large.local_state[4],
			large.local_state[5], ((struct unit_datum *)object)->unit.throttle.i,
			((struct unit_datum *)object)->unit.throttle.j, large.local_airborne ? 1 : 0);
	}
}

/* ---------- fighting */

/* the server's say of a player's health and shields, put into the engine's unit of the player: the engine's
HUD (the local player's) and the effects of the shields (everyone's) read the unit's vitality. A shield or
health that has gone down since the last tick flashes the shield's bubble as a hit does (the engine's
own decay then takes it away again) */
static void large_mode_show_vitals(
	long unit_index,
	unsigned long player)
{
	float vitals[6];
	struct damage_network_state state;
	struct object_datum *object = (struct object_datum *)object_try_and_get_and_verify_type(unit_index,
		_object_mask_unit);
	real shield_before;
	real body_before;
	long flags;

	if (!object || !halo_large_vitals(player, vitals))
		return;
	damage_get_network_state(unit_index, &state);
	shield_before = state.shield_vitality;
	body_before = state.body_vitality;
	flags = (long)(vitals[3] + 0.5f);
	state.shield_depleted = (flags & 1) != 0;
	state.shield_charging = (flags & 8) != 0;
	state.shield_over_charging = (flags & 4) != 0;
	state.shield_vitality = vitals[0];
	/* (a player who is dead is killed as the engine kills one, not by a health below nothing) */
	state.body_vitality = MAX(vitals[1], 0.0f);
	if (state.shield_vitality < shield_before)
	{
		state.current_shield_damage = 1.0f;
		state.recent_shield_damage = MIN(state.recent_shield_damage + (shield_before - state.shield_vitality), 1.0f);
		object->object.shield_damage_decay_timer = 0;
	}
	if (state.body_vitality < body_before)
	{
		state.current_body_damage = MIN(state.current_body_damage + (body_before - state.body_vitality), 1.0f);
		state.recent_body_damage = MIN(state.recent_body_damage + (body_before - state.body_vitality), 1.0f);
		object->object.body_damage_decay_timer = 0;
	}
	damage_set_network_state(unit_index, &state);
}

/* the local player's unit holds the weapon the server says the player carries, in place of what the
engine gave it (its biped's own, whatever the game's starting equipment is: the server's choice is not the
engine's). The weapon is given first, and the player's controls are told to hold it; the others are taken
out of the inventory once it is in hand. FALSE while it is to be tried again: the server has not said
yet, the map is not in, or the weapon is not in hand yet */
static boolean large_mode_equip_local(
	long unit_index)
{
	unsigned long weapons[2];
	char name[160];
	char *dot;
	long definition_index;
	long weapon_index;
	struct object_placement_data placement;
	struct unit_datum *unit = (struct unit_datum *)object_try_and_get_and_verify_type(unit_index, _object_mask_unit);

	if (!unit)
		return FALSE;
	if (large.equip_weapon != NONE)
	{
		/* given: once it is the one in hand, the others go */
		short slot;

		if (!object_try_and_get_and_verify_type(large.equip_weapon, _object_mask_weapon))
			return TRUE;
		if (unit->unit.current_weapon_index == NONE ||
			unit->unit.weapon_object_indices[unit->unit.current_weapon_index] != large.equip_weapon)
		{
			return FALSE;
		}
		for (slot = 0; slot < MAXIMUM_WEAPONS_PER_UNIT; slot++)
		{
			long other = unit->unit.weapon_object_indices[slot];

			if (other != NONE && other != large.equip_weapon)
			{
				object_delete(other);
				unit->unit.weapon_object_indices[slot] = NONE;
			}
		}
		large.equip_weapon = NONE;
		return TRUE;
	}
	if (!halo_large_loadout(large.player_id, weapons))
		return FALSE;
	if (weapons[0] == 0xFFFFUL)
		return TRUE;
	if (!halo_large_weapon_name(weapons[0], name, sizeof(name)))
		return FALSE;
	/* (the engine finds a tag by its name without the group's extension) */
	dot = strrchr(name, '.');
	if (dot)
		*dot = 0;
	definition_index = tag_loaded(WEAPON_DEFINITION_TAG, name);
	if (definition_index == NONE)
	{
		platform_log("large mode: the server's weapon %s is not a tag of the game", name);
		return TRUE;
	}
	object_placement_data_new(&placement, definition_index, unit_index);
	weapon_index = object_new(&placement);
	if (weapon_index == NONE || !unit_add_weapon_to_inventory(unit_index, weapon_index, _unit_add_weapon_starting))
	{
		if (weapon_index != NONE)
			object_delete(weapon_index);
		platform_log("large mode: the local unit could not be given %s", name);
		return TRUE;
	}
	large.equip_weapon = weapon_index;
	platform_log("large mode: the local unit is given %s", name);
	return FALSE;
}

/* large.autofire: the local player looks at the nearest other player in the world, from the eye to the
middle of the body, and holds the trigger */
static void large_mode_autofire(
	struct unit_datum *unit)
{
	struct large_remote const *nearest = NULL;
	real nearest_distance = 0.0f;
	long id;
	real_vector3d aim;
	real dx, dy, dz, length;

	for (id = 0; id < LARGE_MAXIMUM_REMOTES; id++)
	{
		struct large_remote const *remote = &large_remote_data.remotes[id];
		real distance;

		if (!remote->present)
			continue;
		distance = (remote->state[0] - unit->object.position.x) * (remote->state[0] - unit->object.position.x) +
			(remote->state[1] - unit->object.position.y) * (remote->state[1] - unit->object.position.y);
		if (!nearest || distance < nearest_distance)
		{
			nearest = remote;
			nearest_distance = distance;
		}
	}
	if (!nearest)
		return;
	dx = nearest->state[0] - unit->object.position.x;
	dy = nearest->state[1] - unit->object.position.y;
	dz = (nearest->state[2] + 0.2f) - (unit->object.position.z + 0.6f);
	length = (real)sqrt(dx * dx + dy * dy + dz * dz);
	if (length < 0.01f)
		return;
	aim.i = dx / length;
	aim.j = dy / length;
	aim.k = dz / length;
	player_control_set_facing(0, &aim);
	SET_FLAG(unit->unit.control_flags, _unit_control_weapon_primary_trigger_bit, TRUE);
	unit->unit.primary_trigger = 1.0f;
}

/* whether the engine deals this damage (object_cause_damage asks): not to a player's unit in this mode,
whose health and shields are the server's. A hit of the local player's weapon on another player is
reported to the server, which checks it and deals the damage; everything else that would hurt a player
is nothing here (the server decides falls and deaths). The server's own kills of the local player (the
unit_kill of large_mode_game_tick) and everything that is no player's unit (scenery, items) go on as the
engine has them. */
boolean large_mode_damage_deals(
	struct damage_data const *damage,
	long object_index,
	short material_index)
{
	long slot;
	struct unit_datum *shooter;
	long weapon_definition = NONE;
	real_point3d position;
	unsigned long reported;

	if (!large.started || TEST_FLAG(damage->flags, _damage_kill_instantly_bit))
		return TRUE;
	if (!object_try_and_get_and_verify_type(object_index, _object_mask_biped))
		return TRUE;
	slot = DATUM_INDEX_TO_ABSOLUTE_INDEX(object_index);
	if (damage->owner_player_index == NONE || damage->owner_player_index != local_player_get_player_index(0) ||
		slot < 0 || slot >= HALO_PORT_MAXIMUM_OBJECTS_PER_MAP || large_remote_data.remote_of_object[slot] <= 0)
	{
		return FALSE;
	}
	/* a shot of the local player's weapon at another player */
	shooter = (struct unit_datum *)object_try_and_get_and_verify_type(damage->owner_object_index, _object_mask_unit);
	if (shooter && shooter->unit.current_weapon_index != NONE)
	{
		long weapon_index = unit_inventory_get_weapon(damage->owner_object_index,
			(word)shooter->unit.current_weapon_index);

		if (weapon_index != NONE)
			weapon_definition = weapon_get(weapon_index)->definition_index;
	}
	if (weapon_definition == NONE)
		return FALSE;
	object_get_origin(object_index, &position);
	reported = halo_large_report_hit((unsigned long)(large_remote_data.remote_of_object[slot] - 1),
		(unsigned long)DATUM_INDEX_TO_ABSOLUTE_INDEX(weapon_definition), (long)material_index, damage->origin.x,
		damage->origin.y, damage->origin.z, position.x, position.y, position.z);
	if (large.reports_logged < 5)
	{
		large.reports_logged++;
		platform_log("large mode: a hit on player %ld (part %ld) is reported%s: impact (%.3f %.3f %.3f), the player at "
			"(%.3f %.3f %.3f)", (long)(large_remote_data.remote_of_object[slot] - 1), (long)material_index,
			reported ? "" : " (not sent)", damage->origin.x, damage->origin.y, damage->origin.z, position.x,
			position.y, position.z);
	}
	return FALSE;
}

/* ---------- the server's say of the local player's life, and the scoreboard */

/* the team the server says the local player is on, which the engine's player
and unit have (the engine gave it the red team, and the roster alternates) */
static void large_mode_set_local_team(
	struct unit_datum *unit,
	unsigned long team)
{
	long player_index = local_player_get_player_index(0);

	if (player_index != NONE && team < 2 && player_get(player_index)->team_index != (short)team)
		player_get(player_index)->team_index = (short)team;
	if (team < 2 && unit->object.owner_team_index != (short)team)
		unit->object.owner_team_index = (short)team;
}

/* the library's text, as the engine's wide characters */
static void large_mode_widen(
	wchar_t *to,
	char const *from,
	long size)
{
	long index;

	for (index = 0; index < size - 1 && from[index]; index++)
		to[index] = (wchar_t)((byte)from[index] < 0x80 ? from[index] : '?');
	to[index] = 0;
}

/* a change of the local player's life is logged (the automated tests read it) */
static void large_mode_log_life(
	unsigned long const *life)
{
	long state = (long)life[0] * 1000 + (long)(life[2] & 0xFF);

	if (large.life_logged == state)
		return;
	large.life_logged = state;
	platform_log("large mode: the server says the local player is %s (spawns %lu, score %ld, deaths %lu, team %lu, "
		"tick %lu, due %lu)", life[0] == _large_life_alive ? "alive" : life[0] == _large_life_dead ? "dead" : "waiting",
		life[2], (long)life[3], life[4], life[5], life[6], life[1]);
}

/* whether the engine need not choose a starting location for the player (find_best_starting_location_index):
the local player's is the server's to choose, and its unit is put there once the engine has made it */
boolean large_mode_player_spawn_anywhere(
	long player_index)
{
	return large.started && player_index != NONE && player_index == local_player_get_player_index(0);
}

/* the gate of the engine's spawning (game_engine_should_spawn_player): the
local player's unit is the server's to allow, once it says the player is alive;
returns whether the server decides, and *spawn what it says */
boolean large_mode_player_spawn(
	long player_index,
	boolean *spawn)
{
	unsigned long life[8];
	float position[4];

	if (!large.started || player_index == NONE || player_index != local_player_get_player_index(0))
		return FALSE;
	*spawn = halo_large_life(life, position) != 0 && life[0] == _large_life_alive;
	return TRUE;
}

/* what the HUD says of a local player who is not in the world (game_engine_get_state_message):
how long until the respawn, or the wave when no starting location is free */
boolean large_mode_state_message(
	long player_index,
	wchar_t *buffer,
	long count)
{
	unsigned long life[8];
	float position[4];
	long seconds;

	if (!large.started || player_index == NONE || player_index != local_player_get_player_index(0) ||
		!halo_large_life(life, position) || life[0] == _large_life_alive)
	{
		return FALSE;
	}
	/* (the ticks of the server's clock, which runs 30 a second: the difference is a signed number) */
	seconds = ((long)(life[1] - life[6]) + TICKS_PER_SECOND - 1) / TICKS_PER_SECOND;
	seconds = MAX(seconds, 0);
	if (life[0] == _large_life_waiting)
	{
		usnprintf(buffer, count, L"No spawn point is free: respawn wave in %ld seconds", seconds);
	}
	else
	{
		usnprintf(buffer, count, L"You will respawn in %ld seconds", seconds);
	}
	return TRUE;
}

/* whether the game's scoreboard is the mode's (the engine's players are only the nearest 127) */
boolean large_mode_scoreboard_active(
	void)
{
	return large.started;
}

/* whether the game has teams (Team Slayer: the scoreboard has a column for each) */
boolean large_mode_scoreboard_teams(
	void)
{
	unsigned long game[10];

	return halo_large_game(game) != 0 && game[0] != 0;
}

/* whether the scoreboard stays up: the game has ended (the winner is on it, until the next match
follows), or large.scoreboard says so (for the automated tests' pictures) */
boolean large_mode_scoreboard_forced(
	void)
{
	unsigned long game[10];

	if (!large.started)
		return FALSE;
	return large.scoreboard_always || (halo_large_game(game) != 0 && game[7] != 0);
}

/* every player of the match, best first: how many, and then each by its place in the list */
long large_mode_scoreboard_freeze(
	void)
{
	return (long)halo_large_scoreboard_freeze();
}

/* a row: the player, team, score, deaths, life (0 alive, 1 dead, 2 waiting), place, and 1 for the
local player, in values; the name */
boolean large_mode_scoreboard_row(
	long index,
	long *values,
	wchar_t *name,
	long name_size)
{
	unsigned long row[7];
	char text[LARGE_NAME_LENGTH + 1];
	long item;

	if (!halo_large_scoreboard_row((unsigned long)index, row, text, sizeof(text)))
		return FALSE;
	for (item = 0; item < 7; item++)
		values[item] = (long)row[item];
	large_mode_widen(name, text, name_size);
	return TRUE;
}

/* the scoreboard's title: the game, its limit and what the scores are; when the game has ended, who won */
void large_mode_scoreboard_title(
	wchar_t *buffer,
	long size)
{
	unsigned long game[10];
	unsigned long life[8];
	float position[4];
	wchar_t limit[64];
	wchar_t clock[32];

	if (!halo_large_game(game))
	{
		usnprintf(buffer, size, L"Slayer");
		return;
	}
	limit[0] = 0;
	if (game[1])
		usnprintf(limit, NUMBEROF(limit), L"   first to %lu", game[1]);
	clock[0] = 0;
	if (game[2] && !game[7] && halo_large_life(life, position))
	{
		long left = (long)(game[3] + game[2] - life[6]);

		left = MAX(left, 0) / TICKS_PER_SECOND;
		usnprintf(clock, NUMBEROF(clock), L"   %ld:%02ld left", left / 60, left % 60);
	}
	if (game[0])
	{
		usnprintf(buffer, size, L"Team Slayer   Red %ld   Blue %ld%s%s", (long)game[5], (long)game[6], limit, clock);
	}
	else
	{
		usnprintf(buffer, size, L"Slayer%s%s", limit, clock);
	}
	if (game[7])
	{
		wchar_t winner[64];
		char name[LARGE_NAME_LENGTH + 1];
		unsigned long team;

		if (game[8] == 2)
		{
			usnprintf(winner, NUMBEROF(winner), L"the %s team wins", game[9] == 0 ? L"red" : L"blue");
		}
		else if (game[8] == 1 && halo_large_member(game[9], &team, name, sizeof(name)))
		{
			wchar_t wide[LARGE_NAME_LENGTH + 1];

			large_mode_widen(wide, name, NUMBEROF(wide));
			usnprintf(winner, NUMBEROF(winner), L"%s wins", wide);
		}
		else
		{
			usnprintf(winner, NUMBEROF(winner), L"nobody wins");
		}
		usnprintf(buffer, size, L"Game over: %s   (%s)", winner, game[7] == 1 ? L"score limit" : L"time limit");
	}
}

/* called from game_tick, before objects_update */
void large_mode_game_tick(
	void)
{
	long unit_index;
	struct unit_datum *unit;
	unsigned long life[8];
	float life_position[4];
	boolean have_life;

	if (!large.started || !game_engine_running())
		return;

	unit = large_mode_local_unit(&unit_index);
	have_life = halo_large_life(life, life_position) != 0;
	{
		unsigned long status[8];

		halo_large_status(status);
		large.player_id = status[6];
	}
	if (have_life)
		large_mode_log_life(life);
	if (have_life && life[0] != _large_life_alive)
	{
		/* dead, or waiting for a wave: the server says. A unit the engine still has is killed
		(once), and the engine's own respawn is gated (large_mode_player_spawn) */
		large.local_moving = FALSE;
		large.equip_weapon = NONE;
		if (unit && large.killed_unit != unit_index)
		{
			large.killed_unit = unit_index;
			unit_kill(unit_index);
			platform_log("large mode: the server says the local player is %s: the unit is killed",
				life[0] == _large_life_waiting ? "waiting for a wave" : "dead");
		}
	}
	else if (unit && have_life)
	{
		large_mode_set_local_team(unit, life[5]);
		/* the server decides where the player spawns: a unit the engine has made (it spawns
		the player's unit where it chooses, and it is not that) is put where the server
		spawned the player, facing as it says, and the library starts from there; each time
		the server spawns the player, and for any new unit */
		if (large.local_seen_unit != unit_index || large.spawn_seen != life[2])
		{
			real_point3d position;
			real_vector3d forward, up;

			position.x = life_position[0];
			position.y = life_position[1];
			position.z = life_position[2];
			forward.i = (real)cos(life_position[3]);
			forward.j = (real)sin(life_position[3]);
			forward.k = 0.0f;
			up.i = 0.0f;
			up.j = 0.0f;
			up.k = 1.0f;
			object_set_position(unit_index, &position, &forward, &up);
			player_control_set_facing(0, &forward);
			unit->object.translational_velocity.i = 0.0f;
			unit->object.translational_velocity.j = 0.0f;
			unit->object.translational_velocity.k = 0.0f;
			halo_large_place(life_position[0], life_position[1], life_position[2]);
			large.local_seen_unit = unit_index;
			large.spawn_seen = life[2];
			large.placed = TRUE;
			platform_log("large mode: the local unit is where the server has the player: (%.3f %.3f %.3f) spawn %lu",
				life_position[0], life_position[1], life_position[2], life[2]);
		}
		else
		{
			large_mode_move_local(unit_index, unit);
		}
		/* the weapon the server says the player carries, and their health and shields */
		if (large.equipped_unit != unit_index)
		{
			if (large_mode_equip_local(unit_index))
			{
				large.equipped_unit = unit_index;
				platform_log("large mode: the local unit holds the server's weapon");
			}
		}
		large_mode_show_vitals(unit_index, large.player_id);
		if (large.autofire)
			large_mode_autofire(unit);
	}

	large_mode_update_remotes();

	if (game_time_get() - large.logged_time >= TICKS_PER_SECOND)
	{
		large.logged_time = game_time_get();
		large_mode_log();
		large_mode_log_error();
	}
}

/* ---------- the server list */

/* the library's text of one server, empty when there is none */
static void large_mode_text(
	unsigned long index,
	unsigned long field,
	char *buffer,
	size_t size)
{
	if (!halo_large_browse_text(index, field, buffer, (unsigned long)size))
		buffer[0] = 0;
}

/* the list starts with the first frame that asks for it: it is a connection
of its own to the root database, as the player's own identity (the token is
kept in the save root, one file for each SpacetimeDB) */
static void large_mode_browse_start(
	void)
{
	char folder[320];

	if (large.browsing)
		return;
	snprintf(folder, sizeof(folder), "%s/u/large_identity", platform_save_root());
	halo_large_identity_dir(folder);
	halo_large_set_name(config_string("large.name"));
	large.browsing = halo_large_browse_start(large.spacetimedb, large.root) != 0;
	if (!large.browsing)
		large_mode_log_error();
}

/* "servers": every server, with what it is playing and how full it is */
static void large_mode_command_servers(
	void)
{
	unsigned long status[2];
	unsigned long count;
	unsigned long index;
	char message[256];
	char identity[96];

	large_mode_browse_start();
	if (!halo_large_browse_status(status))
	{
		if (halo_large_browse_message(message, sizeof(message)))
			console_warning("the server list is not up yet: %s", message);
		else
			console_warning("the server list is not up yet: connecting to %s", large.spacetimedb);
		return;
	}
	if (status[0] && halo_large_browse_message(message, sizeof(message)))
		console_warning("%s", message);
	if (halo_large_identity(identity, sizeof(identity)))
		console_printf(FALSE, "you are %s", identity);
	count = halo_large_browse_list();
	if (!count)
	{
		console_printf(FALSE, "no servers are listed");
		return;
	}
	console_printf(FALSE, "   server                  map          game type    players");
	for (index = 0; index < count; index++)
	{
		unsigned long numbers[k_large_entry_count];
		char id[64], title[96], map[64], game_type[32];

		if (!halo_large_browse_entry(index, numbers))
			continue;
		large_mode_text(index, _large_text_id, id, sizeof(id));
		large_mode_text(index, _large_text_title, title, sizeof(title));
		large_mode_text(index, _large_text_map, map, sizeof(map));
		large_mode_text(index, _large_text_game_type, game_type, sizeof(game_type));
		if (numbers[_large_entry_joinable])
		{
			console_printf(FALSE, "%c%lu  %-22.22s  %-11.11s  %-11.11s  %lu/%lu", !strcmp(id, large.server_id) ? '*' : ' ',
				index + 1, title, map, game_type, numbers[_large_entry_players], numbers[_large_entry_capacity]);
		}
		else
		{
			console_printf(FALSE, "%c%lu  %-22.22s  between matches", !strcmp(id, large.server_id) ? '*' : ' ', index + 1,
				title);
		}
	}
	console_printf(FALSE, "join <number> to play on one");
}

/* ends the session and the mode's hold on the game (the player stays where
they are: the lobby or the game, as an ordinary one of one machine) */
static void large_mode_leave(
	void)
{
	if (large.started)
	{
		halo_large_stop();
		large.started = FALSE;
	}
	large.active = FALSE;
	large.phase = _large_phase_idle;
	large.server_id[0] = 0;
	large.entered_game = FALSE;
	network_test_large_stop();
}

/* takes a seat on the server at this index of the frozen list: the session
starts at once, and the game hosts the server's map when it has one */
static void large_mode_begin_join(
	unsigned long index)
{
	char map[64], game_type[32];

	large_mode_text(index, _large_text_id, large.server_id, sizeof(large.server_id));
	large_mode_text(index, _large_text_title, large.server_title, sizeof(large.server_title));
	large_mode_text(index, _large_text_map, large.map, sizeof(large.map));
	large_mode_text(index, _large_text_gateway, large.gateway, sizeof(large.gateway));
	large_mode_text(index, _large_text_database, large.database, sizeof(large.database));
	large_mode_text(index, _large_text_map, map, sizeof(map));
	large_mode_text(index, _large_text_game_type, game_type, sizeof(game_type));
	if (large.started)
		halo_large_stop();
	large.active = TRUE;
	large.placed = FALSE;
	large.logged_error[0] = 0;
	large.told_refusal[0] = 0;
	large.logged_time = 0;
	large.entered_game = FALSE;
	large.started = halo_large_start(large.gateway, large.spacetimedb, large.database) != 0;
	if (!large.started)
	{
		large_mode_log_error();
		console_warning("could not join %s", large.server_title);
		large_mode_leave();
		return;
	}
	large_mode_load_map();
	large.phase = _large_phase_joining;
	large.phase_seconds = 0.0f;
	console_printf(FALSE, "joining %s: %s, %s", large.server_title, map, game_type);
	platform_log("large mode: joining %s (%s), database %s, gateway %s", large.server_id, map, large.database,
		large.gateway);
}

/* "join <number or id>" */
static void large_mode_command_join(
	char const *argument)
{
	unsigned long count;
	unsigned long index;
	unsigned long numbers[k_large_entry_count];

	large_mode_browse_start();
	count = halo_large_browse_list();
	if (!argument[0])
	{
		console_warning("join <number>: the number of a server of the list (servers)");
		return;
	}
	if (argument[0] >= '0' && argument[0] <= '9')
		index = (unsigned long)atol(argument) - 1;
	else
		index = halo_large_browse_find(argument) - 1;
	if (index >= count || !halo_large_browse_entry(index, numbers))
	{
		console_warning("no server %s in the list (servers)", argument);
		return;
	}
	if (!numbers[_large_entry_joinable])
	{
		console_warning("that server is between matches: try again in a moment");
		return;
	}
	if (large.phase == _large_phase_playing && large.entered_game)
	{
		/* in a game of another server: its game ends (its scores are shown, as at
		the end of any), and the lobby joins this one */
		large_mode_text(index, _large_text_id, large.server_id, sizeof(large.server_id));
		large_mode_text(index, _large_text_title, large.server_title, sizeof(large.server_title));
		console_printf(FALSE, "leaving the game to join %s", large.server_title);
		halo_large_stop();
		large.started = FALSE;
		large.phase = _large_phase_moving;
		large.phase_seconds = 0.0f;
		large.entered_game = FALSE;
		network_test_large_leave_game();
		return;
	}
	large_mode_begin_join(index);
}

/* the developer console's lines that are this mode's (hs.c hands each over
before it reads it as a script): returns whether it was one. Only with a
server list: otherwise "join" and "servers" are nothing of ours. */
boolean large_mode_console_command(
	char const *expression)
{
	char line[128];
	char *word;
	char *argument;
	size_t length;

	if (!large.checked)
		large_mode_read_settings();
	if (!large.browser_mode)
		return FALSE;
	while (*expression == ' ' || *expression == '\t' || *expression == '(')
		expression++;
	snprintf(line, sizeof(line), "%s", expression);
	length = strlen(line);
	while (length > 0 && (line[length - 1] == ' ' || line[length - 1] == '\t' || line[length - 1] == ')' ||
		line[length - 1] == '"'))
	{
		line[--length] = 0;
	}
	word = line;
	argument = line;
	while (*argument && *argument != ' ' && *argument != '\t')
		argument++;
	if (*argument)
		*argument++ = 0;
	while (*argument == ' ' || *argument == '\t' || *argument == '"')
		argument++;

	if (!strcmp(word, "servers"))
		large_mode_command_servers();
	else if (!strcmp(word, "join"))
		large_mode_command_join(argument);
	else if (!strcmp(word, "leave"))
	{
		if (large.active)
			console_printf(FALSE, "left %s", large.server_title);
		else
			console_printf(FALSE, "not on a server");
		large_mode_leave();
	}
	else
		return FALSE;
	return TRUE;
}

/* a server whose list entry names another match than the one the player is
in has moved on: back to the lobby, and join the new one */
static void large_mode_follow(
	boolean main_menu_loaded)
{
	unsigned long at, numbers[k_large_entry_count];
	char database[128];

	if (!halo_large_browse_list())
		return;
	at = halo_large_browse_find(large.server_id);
	if (!at || !halo_large_browse_entry(at - 1, numbers) || !numbers[_large_entry_joinable])
		return;
	large_mode_text(at - 1, _large_text_database, database, sizeof(database));
	if (!strcmp(database, large.database))
		return;
	console_printf(FALSE, "%s has a new match: moving on", large.server_title);
	platform_log("large mode: %s moved from %s to %s", large.server_id, large.database, database);
	/* the old match's session ends; the game goes back to the lobby, and the next
	frame there takes a seat in the new one */
	halo_large_stop();
	large.started = FALSE;
	large.phase = _large_phase_moving;
	large.phase_seconds = 0.0f;
	large.entered_game = FALSE;
	if (main_menu_loaded)
		return;
	network_test_large_leave_game();
}

/* every frame (from the main loop, network_test.c): the player's join, and a
server's rotation */
void large_mode_update(
	boolean main_menu_loaded,
	real seconds)
{
	unsigned long status[8];
	unsigned long kind;
	char message[256];
	boolean playing;

	if (!large.checked)
		large_mode_read_settings();
	if (!large.browser_mode)
		return;
	large_mode_browse_start();
	switch (large.phase)
	{
	case _large_phase_joining:
		large.phase_seconds += seconds;
		halo_large_status(status);
		kind = halo_large_refusal(message, sizeof(message));
		if (kind)
		{
			/* banned, or full: the player is told once, and a full server is waited on */
			if (strcmp(message, large.told_refusal))
			{
				snprintf(large.told_refusal, sizeof(large.told_refusal), "%s", message);
				console_warning("%s", message);
				platform_log("large mode: %s", message);
			}
			if (kind == _large_refusal_banned || large.phase_seconds > LARGE_JOIN_PATIENCE)
			{
				if (kind != _large_refusal_banned)
					console_warning("giving up on %s", large.server_title);
				large_mode_leave();
			}
		}
		else if (status[6] != 0xFFFFFFFFUL)
		{
			/* a seat: the game hosts the server's map, and the library brings the others */
			large.phase = _large_phase_playing;
			console_printf(FALSE, "joined %s as player %lu: loading %s", large.server_title, status[6], large.map);
			network_test_large_host(large.map);
		}
		else if (large.phase_seconds > LARGE_JOIN_PATIENCE)
		{
			if (halo_large_error(message, sizeof(message)))
				console_warning("could not join %s: %s", large.server_title, message);
			else
				console_warning("could not join %s", large.server_title);
			large_mode_leave();
		}
		break;
	case _large_phase_playing:
		playing = !main_menu_loaded && game_in_progress();
		if (playing)
			large.entered_game = TRUE;
		else if (large.entered_game)
		{
			/* the game is over (the player went back to the menu): off the server */
			console_printf(FALSE, "left %s", large.server_title);
			large_mode_leave();
			break;
		}
		/* banned while playing: the match took the player's seat, and its next
		join says why. The game ends (its scores are shown, the player goes on
		from them as at the end of any game) and the player is off the server. */
		kind = halo_large_refusal(message, sizeof(message));
		if (kind == _large_refusal_banned)
		{
			console_warning("%s", message);
			platform_log("large mode: %s", message);
			network_test_large_leave_game();
			large_mode_leave();
			break;
		}
		large.followed_seconds += seconds;
		if (large.followed_seconds >= 1.0f)
		{
			large.followed_seconds = 0.0f;
			large_mode_follow(main_menu_loaded);
		}
		break;
	case _large_phase_moving:
		/* back in the lobby: the server's next match (which a server gone from the list, or
		long between matches, does not have) */
		large.phase_seconds += seconds;
		if (large.phase_seconds > 6.0f * LARGE_JOIN_PATIENCE)
		{
			console_warning("%s has no match to join: giving up", large.server_title);
			large_mode_leave();
			break;
		}
		if (main_menu_loaded)
		{
			unsigned long count = halo_large_browse_list();
			unsigned long at = halo_large_browse_find(large.server_id);
			unsigned long numbers[k_large_entry_count];

			if (at && at <= count && halo_large_browse_entry(at - 1, numbers) && numbers[_large_entry_joinable])
				large_mode_begin_join(at - 1);
		}
		break;
	}
}

#else

/* no library in this build: the mode is off, and a setting for it said */
boolean large_mode_active(
	void)
{
#ifndef HALO_ANDROID
	static boolean said = FALSE;

	if (!said)
	{
		said = TRUE;
		if (config_string("large.map")[0])
			platform_log("large mode: this build has no large-scale mode library; the game is played as usual");
	}
#endif
	return FALSE;
}

char const *large_mode_map(
	void)
{
	return "";
}

void large_mode_new_game(
	void)
{
}

void large_mode_dispose(
	void)
{
}

void large_mode_game_tick(
	void)
{
}

void large_mode_update(
	boolean main_menu_loaded,
	real seconds)
{
}

boolean large_mode_console_command(
	char const *expression)
{
	return FALSE;
}

void large_mode_game_tick_after_objects(
	void)
{
}

boolean large_mode_damage_deals(
	struct damage_data const *damage,
	long object_index,
	short material_index)
{
	return TRUE;
}

boolean large_mode_remote_player(
	long player_index)
{
	return FALSE;
}

boolean large_mode_biped_state(
	long biped_index,
	boolean *airborne,
	real *landing_velocity)
{
	return FALSE;
}

boolean large_mode_player_spawn(
	long player_index,
	boolean *spawn)
{
	return FALSE;
}

boolean large_mode_player_spawn_anywhere(
	long player_index)
{
	return FALSE;
}

boolean large_mode_state_message(
	long player_index,
	wchar_t *buffer,
	long count)
{
	return FALSE;
}

boolean large_mode_scoreboard_active(
	void)
{
	return FALSE;
}

boolean large_mode_scoreboard_forced(
	void)
{
	return FALSE;
}

boolean large_mode_scoreboard_teams(
	void)
{
	return FALSE;
}

long large_mode_scoreboard_freeze(
	void)
{
	return 0;
}

boolean large_mode_scoreboard_row(
	long index,
	long *values,
	wchar_t *name,
	long name_size)
{
	return FALSE;
}

void large_mode_scoreboard_title(
	wchar_t *buffer,
	long size)
{
}

#endif
