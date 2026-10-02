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
holds from what the gateway sends. In this ticket the other players are not
drawn, and the local player does not move: the game logs what the library
holds, once a second, for the automated test to compare with what the server
sent.

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
the save root (u/large_identity), one file for each SpacetimeDB.

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
unsigned long halo_large_local(float *out);
unsigned long halo_large_bounds(float *out);
void halo_large_send_input(float x, float y, float z, float yaw, float pitch);
unsigned long halo_large_error(char *buffer, unsigned long size);
void halo_large_identity_dir(const char *folder);
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
	/* (with a server list the session is the player's join's, which started it) */
	if (large.browser_mode)
		return;
	large.started = halo_large_start(large.gateway, large.spacetimedb, large.database) != 0;
	if (!large.started)
		large_mode_log_error();
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
		unsigned long numbers[5];
		char id[64], title[96], map[64], game_type[32];

		if (!halo_large_browse_entry(index, numbers))
			continue;
		large_mode_text(index, _large_text_id, id, sizeof(id));
		large_mode_text(index, _large_text_title, title, sizeof(title));
		large_mode_text(index, _large_text_map, map, sizeof(map));
		large_mode_text(index, _large_text_game_type, game_type, sizeof(game_type));
		if (numbers[4])
		{
			console_printf(FALSE, "%c%lu  %-22.22s  %-11.11s  %-11.11s  %lu/%lu", !strcmp(id, large.server_id) ? '*' : ' ',
				index + 1, title, map, game_type, numbers[0], numbers[1]);
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
	unsigned long numbers[5];

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
	if (!numbers[4])
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
	unsigned long at, numbers[5];
	char database[128];

	if (!halo_large_browse_list())
		return;
	at = halo_large_browse_find(large.server_id);
	if (!at || !halo_large_browse_entry(at - 1, numbers) || !numbers[4])
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
			if (kind == 1 || large.phase_seconds > LARGE_JOIN_PATIENCE)
			{
				if (kind != 1)
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
		if (kind == 1)
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
		/* back in the lobby: the server's next match */
		if (main_menu_loaded)
		{
			unsigned long count = halo_large_browse_list();
			unsigned long at = halo_large_browse_find(large.server_id);
			unsigned long numbers[5];

			if (at && at <= count && halo_large_browse_entry(at - 1, numbers) && numbers[4])
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

#endif
