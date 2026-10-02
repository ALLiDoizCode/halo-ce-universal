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
  local player's unit is put where the server has it (once), and its position
  and facing go to the gateway as that tick's input;
- when the game ends (large_mode_dispose) the library stops.

What the mode switches off: the distributed netcode's tick and message handling
(network_distributed.c, which a game of one machine would only idle through);
the C engine does not simulate other players, whom the library
holds from what the gateway sends. Each of them is drawn by the existing
renderer (see "remote players" below), and the local player does not move yet:
the game logs what the library holds, and where it has drawn each remote
player, once a second, for the automated test to compare with what the server
sent.

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

Without HALO_LARGE_MODE (the Android build, or a desktop build made without
the library) the mode is not there: large_mode_active() is FALSE.
*/

#include "cseries.h"
#include "game/game.h"
#include "game/game_engine.h"
#include "game/game_globals.h"
#include "game/players.h"
#include "objects/objects.h"
#include "scenario/scenario.h"
#include "units/unit_control_data.h"
#include "units/units.h"

#include <math.h>
#include <stdlib.h>
#include <string.h>

/* the platform layer's (port/linux/src/port_config.c) */
const char *config_string(char const *name);
long config_integer(char const *name);
int config_boolean(char const *name);
void platform_log(char const *format, ...);

#ifdef HALO_LARGE_MODE

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
unsigned long halo_large_bounds(float *out);
void halo_large_send_input(float x, float y, float z, float yaw, float pitch);
unsigned long halo_large_error(char *buffer, unsigned long size);

static struct
{
	boolean checked;
	boolean active;
	char map[64];
	char gateway[128];
	char spacetimedb[256];
	char database[128];
	boolean log_players;

	/* running: the library has been started for this game, the local player's
	unit has been put where the server has it, and the game tick the status was
	last logged on */
	boolean started;
	boolean placed;
	char logged_error[256];
	long logged_time;
} large;

static void large_mode_read_settings(
	void)
{
	char const *map = config_string("large.map");

	large.checked = TRUE;
	if (!map[0])
		return;
	snprintf(large.map, sizeof(large.map), "%s", map);
	snprintf(large.gateway, sizeof(large.gateway), "%s", config_string("large.gateway"));
	snprintf(large.spacetimedb, sizeof(large.spacetimedb), "%s", config_string("large.spacetimedb"));
	snprintf(large.database, sizeof(large.database), "%s", config_string("large.database"));
	large.log_players = config_boolean("large.log_players") != 0;
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
	large_mode_forget_remotes();
	large.started = halo_large_start(large.gateway, large.spacetimedb, large.database) != 0;
	if (!large.started)
		large_mode_log_error();
}

/* the game is over, or another map is loading */
void large_mode_dispose(
	void)
{
	if (!large.started)
		return;
	halo_large_stop();
	large.started = FALSE;
	large.placed = FALSE;
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
	this tick */
	unsigned long tick;
	float state[8];
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
	if (object_try_and_get_and_verify_type(remote->unit_index, _object_mask_unit))
		object_delete(remote->unit_index);
	remote->present = FALSE;
	large_remote_data.count--;
	large_remote_data.removed++;
	if (large.log_players)
		platform_log("large mode: player %lu is out of range: unit deleted", id);
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

	csmemset(&control, 0, sizeof(control));
	control.animation_state = _unit_animation_state_in_combat;
	control.weapon_index = NONE;
	control.grenade_index = NONE;
	control.zoom_level = NONE;
	/* (the tags' speeds are never zero, but a throttle must never be not a number) */
	control.throttle.i = PIN(ahead / MAX(ahead > 0.0f ? information->run_forward_speed : information->run_backward_speed,
		0.001f), -1.0f, 1.0f);
	control.throttle.j = PIN(left / MAX(information->run_sideways_speed, 0.001f), -1.0f, 1.0f);
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
		float state[8];

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
			large_mode_remove_remote((unsigned long)id);
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
			large_mode_drive_remote(&large_remote_data.remotes[id], information);
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

	if (!large.started || large_remote_data.count <= 0)
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

	platform_log("large mode: %ld remote units, %ld with players | created %ld removed %ld failures %ld",
		large_remote_data.count, large_remote_data.players, large_remote_data.created, large_remote_data.removed,
		large_remote_data.create_failures);
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
		platform_log("large mode: drawn %ld tick %lu (%.4f %.4f %.4f) team %ld player %ld", id, remote->tick,
			object->object.position.x, object->object.position.y, object->object.position.z, remote->team,
			remote->player_index == NONE ? -1L : (long)DATUM_INDEX_TO_ABSOLUTE_INDEX(remote->player_index));
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
	if (!large.log_players)
		return;
	for (index = 0; index < count; index++)
	{
		unsigned long player, unit_tick;
		float state[8];

		if (halo_large_unit(index, &player, &unit_tick, state))
		{
			platform_log("large mode: player %lu tick %lu (%.4f %.4f %.4f) v (%.3f %.3f %.3f) yaw %.4f pitch %.4f",
				player, unit_tick, state[0], state[1], state[2], state[3], state[4], state[5], state[6], state[7]);
		}
	}
}

/* called from game_tick, before objects_update */
void large_mode_game_tick(
	void)
{
	long unit_index;
	struct unit_datum *unit;

	if (!large.started || !game_engine_running())
		return;

	unit = large_mode_local_unit(&unit_index);
	if (unit)
	{
		/* the server decides where the player starts: put the unit there, once */
		if (!large.placed)
		{
			float own[5];

			if (halo_large_local(own))
			{
				real_point3d position;
				real_vector3d forward, up;

				position.x = own[0];
				position.y = own[1];
				position.z = own[2];
				forward.i = (real)cos(own[3]);
				forward.j = (real)sin(own[3]);
				forward.k = 0.0f;
				up.i = 0.0f;
				up.j = 0.0f;
				up.k = 1.0f;
				object_set_position(unit_index, &position, &forward, &up);
				unit->object.translational_velocity.i = 0.0f;
				unit->object.translational_velocity.j = 0.0f;
				unit->object.translational_velocity.k = 0.0f;
				large.placed = TRUE;
				platform_log("large mode: the local unit is where the server has the player: (%.3f %.3f %.3f)",
					own[0], own[1], own[2]);
			}
		}
		else
		{
			/* this tick's input */
			real_vector3d const *aim = &unit->unit.aiming_vector;

			halo_large_send_input(unit->object.position.x, unit->object.position.y, unit->object.position.z,
				(float)atan2(aim->j, aim->i), (float)asin(PIN(aim->k, -1.0f, 1.0f)));
		}
	}

	large_mode_update_remotes();

	if (game_time_get() - large.logged_time >= TICKS_PER_SECOND)
	{
		large.logged_time = game_time_get();
		large_mode_log();
		large_mode_log_error();
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

void large_mode_game_tick_after_objects(
	void)
{
}

boolean large_mode_remote_player(
	long player_index)
{
	return FALSE;
}

#endif
