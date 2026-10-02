/*
SCENARIO_HARNESS.C

The comparison harness (tools/scenario_harness.py, tools/scenarios/): plays a
scripted scenario in the engine without a person and writes a trace of the
player's state, one line a tick, for tools/scenario_harness.py to compare with
another trace (another run of the engine, or the Rust simulation of the
large-scale mode).

debug.scenario (HALO_SCENARIO) names a scenario file; with it the game runs a
network test session on this machine alone (debug.network_test, normally
"host:<the scenario's map>", which tools/scenario_harness.py sets up) and:

- once the local player's unit is in play, puts it at the scenario's start,
  facing as given, at rest, and counts ticks from there;
- from then on replaces the local player's controls (players_update_before_game)
  with the scenario's inputs of that tick: the throttle, the facing, the jump
  and crouch buttons;
- after each tick's update of the objects, records the unit's state as a
  trace line;
- after the scenario's last tick, writes the trace to debug.scenario_trace
  (HALO_SCENARIO_TRACE) and ends the game.

The scenario and trace formats are in tools/scenarios/README.md.
*/

#include "cseries.h"
#include "main/main.h"
#include "game/game.h"
#include "game/game_engine.h"
#include "game/players.h"
#include "objects/objects.h"
#include "units/units.h"

#include <math.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* the host's files, not the game's (stdio.h maps their names to the Xbox's) */
#undef fopen

/* the platform layer's (port/linux/src/port_config.c) */
const char *config_string(char const *name);
void platform_log(char const *format, ...);

/* the game's ticks a second, and the most ticks a scenario may have */
#define SCENARIO_TICKS_PER_SECOND 30
#define SCENARIO_MAXIMUM_TICKS 36000

struct scenario_record
{
	float position[3];
	float velocity[3];
	float yaw;
	float pitch;
	int state;
};

struct scenario_input
{
	float forward;
	float strafe;
	float yaw;
	float pitch;
	boolean jump;
	boolean crouch;
};

static struct
{
	boolean checked;
	boolean active;
	char name[64];
	char map[64];
	float start_x, start_y, start_z, start_yaw;
	long tick_count;
	struct scenario_input inputs[SCENARIO_MAXIMUM_TICKS];
	const char *trace_path;

	/* running: the game's tick that is the scenario's tick 0 (NONE before),
	and the trace so far */
	long first_game_tick;
	long recorded;
	struct scenario_record records[SCENARIO_MAXIMUM_TICKS];
} harness;

static void scenario_fail(
	char const *message,
	char const *detail)
{
	platform_log("scenario: %s%s%s", message, detail ? ": " : "", detail ? detail : "");
	exit(2);
}

/* a "key=value" word of an input line, applied to the inputs of ticks from
up to (not including) to */
static void scenario_apply(
	char const *word,
	long from,
	long to,
	char const *line)
{
	char const *equals = strchr(word, '=');
	char key[16];
	float value;
	long tick;

	if (!equals || equals - word >= (long)sizeof(key))
		scenario_fail("unreadable input", line);
	memcpy(key, word, (size_t)(equals - word));
	key[equals - word] = 0;
	value = (float)atof(equals + 1);
	for (tick = from; tick < to; tick++)
	{
		struct scenario_input *input = &harness.inputs[tick];

		if (!strcmp(key, "forward"))
			input->forward = value;
		else if (!strcmp(key, "strafe"))
			input->strafe = value;
		else if (!strcmp(key, "yaw"))
			input->yaw = value;
		else if (!strcmp(key, "pitch"))
			input->pitch = value;
		else if (!strcmp(key, "jump"))
			input->jump = value != 0.0f;
		else if (!strcmp(key, "crouch"))
			input->crouch = value != 0.0f;
		else
			scenario_fail("unknown input", key);
	}
}

/* the next line of the file without its comment or leading blanks, or NULL at
the end */
static char const *scenario_read_line(
	FILE *file,
	char *line,
	int size)
{
	char *comment;
	char const *start = line;

	if (!fgets(line, size, file))
		return NULL;
	comment = strchr(line, '#');
	if (comment)
		*comment = 0;
	while (*start == ' ' || *start == '\t')
		start++;
	return start;
}

static void scenario_load(
	char const *path)
{
	FILE *file = fopen(path, "r");
	char buffer[512];
	char const *line;
	boolean have_start = FALSE;
	long tick;

	if (!file)
		scenario_fail("cannot open the scenario", path);
	/* (the ticks line comes before the inputs: read the file in two passes,
	for the header first) */
	harness.tick_count = 0;
	while ((line = scenario_read_line(file, buffer, sizeof(buffer))) != NULL)
	{
		char name[64];
		float x, y, z, yaw;
		long count;

		if (sscanf(line, "scenario %63s", harness.name) == 1)
			continue;
		if (sscanf(line, "map %63s", harness.map) == 1)
			continue;
		if (sscanf(line, "start %63s %f %f %f %f", name, &x, &y, &z, &yaw) == 5)
		{
			harness.start_x = x;
			harness.start_y = y;
			harness.start_z = z;
			harness.start_yaw = yaw;
			have_start = TRUE;
			continue;
		}
		if (sscanf(line, "ticks %ld", &count) == 1)
			harness.tick_count = count;
	}
	if (!harness.name[0] || !harness.map[0] || !have_start || harness.tick_count <= 0 ||
		harness.tick_count > SCENARIO_MAXIMUM_TICKS)
	{
		scenario_fail("the scenario needs a name, a map, a start and 1 to 36000 ticks", path);
	}
	for (tick = 0; tick < harness.tick_count; tick++)
		harness.inputs[tick].yaw = harness.start_yaw;

	rewind(file);
	while ((line = scenario_read_line(file, buffer, sizeof(buffer))) != NULL)
	{
		long from, to;
		int offset;

		if (sscanf(line, "input %ld %ld %n", &from, &to, &offset) >= 2)
		{
			char word[64];
			char const *rest = line + offset;
			int used;

			if (from < 0 || to > harness.tick_count || from > to)
				scenario_fail("input ticks out of range", line);
			while (sscanf(rest, "%63s%n", word, &used) == 1)
			{
				scenario_apply(word, from, to, line);
				rest += used;
			}
		}
	}
	fclose(file);
}

static void scenario_read_settings(
	void)
{
	char const *path = config_string("debug.scenario");

	harness.checked = TRUE;
	harness.first_game_tick = NONE;
	if (!path[0])
		return;
	scenario_load(path);
	harness.trace_path = config_string("debug.scenario_trace");
	if (!harness.trace_path[0])
		scenario_fail("debug.scenario needs debug.scenario_trace, where the trace goes", NULL);
	harness.active = TRUE;
	platform_log("scenario: %s on %s, %ld ticks", harness.name, harness.map, harness.tick_count);
}

boolean scenario_harness_active(
	void)
{
	if (!harness.checked)
		scenario_read_settings();
	return harness.active;
}

/* the player the scenario plays: the machine's first */
static struct unit_datum *scenario_unit(
	long player_index)
{
	struct player_datum *player = player_get(player_index);

	if (player->local_player_index != 0 || player->unit_index == NONE)
		return NULL;
	return unit_get(player->unit_index);
}

/* at the start of a tick's player update (players_update_before_game): the
scenario's inputs of the tick for the machine's first player's action */
void scenario_harness_control(
	long player_index,
	struct player_action *action)
{
	struct unit_datum *unit;
	struct scenario_input const *input;
	long tick;

	if (!scenario_harness_active() || harness.recorded >= harness.tick_count)
		return;
	unit = scenario_unit(player_index);
	if (!unit || !game_engine_running() || !game_engine_can_score() || players_globals->input_disabled)
		return;
	if (harness.first_game_tick == NONE)
	{
		real_point3d position;
		real_vector3d forward, up;
		long unit_index = player_get(player_index)->unit_index;

		position.x = harness.start_x;
		position.y = harness.start_y;
		position.z = harness.start_z;
		forward.i = (real)cos(harness.start_yaw);
		forward.j = (real)sin(harness.start_yaw);
		forward.k = 0.0f;
		up.i = 0.0f;
		up.j = 0.0f;
		up.k = 1.0f;
		object_set_position(unit_index, &position, &forward, &up);
		unit->object.translational_velocity.i = 0.0f;
		unit->object.translational_velocity.j = 0.0f;
		unit->object.translational_velocity.k = 0.0f;
		harness.first_game_tick = game_time_get();
		platform_log("scenario: starting at game tick %ld", harness.first_game_tick);
	}
	tick = game_time_get() - harness.first_game_tick;
	if (tick < 0 || tick >= harness.tick_count)
		return;
	input = &harness.inputs[tick];
	action->control_flags = 0;
	if (input->jump)
		SET_FLAG(action->control_flags, _unit_control_jump_bit, TRUE);
	if (input->crouch)
		SET_FLAG(action->control_flags, _unit_control_crouch_modifier_bit, TRUE);
	action->throttle.i = input->forward;
	action->throttle.j = input->strafe;
	action->desired_facing.yaw = input->yaw;
	action->desired_facing.pitch = input->pitch;
	action->primary_trigger = 0.0f;
}

/* after a tick's update of the objects: its trace line, and the end of the
scenario */
void scenario_harness_record(
	void)
{
	struct data_iterator iterator;
	struct player_datum *player;
	struct unit_datum *unit = NULL;
	long tick;
	real_vector3d aim;
	int state = 0;

	if (!scenario_harness_active() || harness.first_game_tick == NONE || harness.recorded >= harness.tick_count)
		return;
	tick = game_time_get() - harness.first_game_tick;
	if (tick != harness.recorded)
		return;
	data_iterator_new(&iterator, player_data);
	while ((player = (struct player_datum *)data_iterator_next(&iterator)) != NULL)
	{
		if (player->local_player_index == 0 && player->unit_index != NONE)
			unit = unit_get(player->unit_index);
	}
	if (!unit)
		scenario_fail("the player's unit is gone", harness.name);

	aim = unit->unit.aiming_vector;
	if (!TEST_FLAG(unit->object.flags, _object_on_ground_bit))
		state |= 1;
	if (unit->unit.animation.base_seat_index == _unit_base_seat_crouch)
		state |= 2;
	{
		struct scenario_record *record = &harness.records[tick];

		record->position[0] = unit->object.position.x;
		record->position[1] = unit->object.position.y;
		record->position[2] = unit->object.position.z;
		/* (world units a second, as the trace format has it) */
		record->velocity[0] = unit->object.translational_velocity.i * SCENARIO_TICKS_PER_SECOND;
		record->velocity[1] = unit->object.translational_velocity.j * SCENARIO_TICKS_PER_SECOND;
		record->velocity[2] = unit->object.translational_velocity.k * SCENARIO_TICKS_PER_SECOND;
		record->yaw = (float)atan2(aim.j, aim.i);
		record->pitch = (float)asin(PIN(aim.k, -1.0f, 1.0f));
		record->state = state;
	}
	harness.recorded++;

	if (harness.recorded >= harness.tick_count)
	{
		FILE *file = fopen(harness.trace_path, "w");

		if (!file)
			scenario_fail("cannot write the trace", harness.trace_path);
		fprintf(file, "# halo-trace 1\n# scenario %s\n# map %s\n# source c-engine\n"
			"tick\tx\ty\tz\tvx\tvy\tvz\tyaw\tpitch\tstate\n", harness.name, harness.map);
		for (tick = 0; tick < harness.tick_count; tick++)
		{
			struct scenario_record const *record = &harness.records[tick];

			fprintf(file, "%ld\t%.6f\t%.6f\t%.6f\t%.6f\t%.6f\t%.6f\t%.6f\t%.6f\t%d\n", tick,
				record->position[0], record->position[1], record->position[2],
				record->velocity[0], record->velocity[1], record->velocity[2],
				record->yaw, record->pitch, record->state);
		}
		fclose(file);
		platform_log("scenario: %s done, %ld ticks written to %s", harness.name, harness.recorded, harness.trace_path);
		exit(EXIT_SUCCESS);
	}
}
