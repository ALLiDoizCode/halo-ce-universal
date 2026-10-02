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
  connects to the gateway over UDP and to SpacetimeDB directly, and the player
  large.player, who must already be in the match, joins;
- every tick, just before the objects are updated (large_mode_game_tick), the
  local player's unit is put where the server has it (once), and its position
  and facing go to the gateway as that tick's input;
- when the game ends (large_mode_dispose) the library stops.

What the mode switches off: the distributed netcode's tick and message handling
(network_distributed.c, which a game of one machine would only idle through);
the C engine does not simulate other players, whom the library
holds from what the gateway sends. In this ticket the other players are not
drawn, and the local player does not move: the game logs what the library
holds, once a second, for the automated test to compare with what the server
sent.

The library's interface (halo_large_*, rust/halo-client/src/ffi.rs) passes
only floats, 32-bit integers and pointers, and returns nothing by value but a
32-bit integer.

Without HALO_LARGE_MODE (the Android build, or a desktop build made without
the library) the mode is not there: large_mode_active() is FALSE.
*/

#include "cseries.h"
#include "game/game.h"
#include "game/game_engine.h"
#include "game/players.h"
#include "objects/objects.h"
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
unsigned long halo_large_start(const char *gateway, const char *spacetimedb, const char *database,
	unsigned long player);
void halo_large_stop(void);
unsigned long halo_large_status(unsigned long *out);
unsigned long halo_large_frame(unsigned long *tick);
unsigned long halo_large_unit(unsigned long index, unsigned long *player, unsigned long *tick, float *out);
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
	long player;
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
	large.player = config_integer("large.player");
	large.log_players = config_boolean("large.log_players") != 0;
	if (!large.database[0] || large.player < 0 || large.player > 0xFFFF)
	{
		platform_log("large mode: large.database names the match's database, and large.player (0 to 65535) the "
			"player to be; neither can be missing: the game is played as usual");
		return;
	}
	large.active = TRUE;
	platform_log("large mode: %s as player %ld of database %s, gateway %s, SpacetimeDB %s", large.map, large.player,
		large.database, large.gateway, large.spacetimedb);
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
	large.started = halo_large_start(large.gateway, large.spacetimedb, large.database,
		(unsigned long)large.player) != 0;
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
	platform_log("large mode: tick %ld joined %d slow %lu map %lu | hellos %lu datagrams %lu bytes %lu ignored %lu "
		"inputs %lu | gateway tick %lu, %lu players",
		game_time_get(), joined ? 1 : 0, status[0], status[1], status[2], status[3], status[4], status[5], status[7],
		tick, count);
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

#endif
