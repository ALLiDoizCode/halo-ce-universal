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

A scenario of firing (it has a weapon or a target line) also puts the named
weapon in the player's hands in place of the ones the game gave, and a target
beside them: a unit of the multiplayer player's kind, standing still and
taking damage as any unit does; the "fire" input holds the player's trigger
and the "melee" input the melee button. The trace then has the shooter's
weapon (its ammunition, heat and how many times it has fired) and the target
(its shields, health and what hit it) after each tick too, and a "# hit" line
for every hit on the target (what hit it, where, at what scale and for how
much), which the simulation replays: what a shot hits is the engine's.

The scenario and trace formats are in tools/scenarios/README.md.
*/

#include "cseries.h"
#include "main/main.h"
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

/* the most ticks a scenario may have */
#define SCENARIO_MAXIMUM_TICKS 36000

/* the trace's movement state bits (tools/scenarios/README.md) */
#define SCENARIO_STATE_AIRBORNE 1
#define SCENARIO_STATE_CROUCHING 2

struct scenario_record
{
	float position[3];
	float velocity[3];
	float yaw;
	float pitch;
	int state;
	/* a scenario of firing: the weapon's rounds loaded and left, its heat, and
	the target's shield and health (a full shield or health is 1), the ticks its
	shield is stunned for, whether it is dead, and the part of it (the index of
	its collision model's materials) that was hit this tick, -1 for none */
	int rounds_loaded;
	int rounds_total;
	float heat;
	float target_shield;
	float target_body;
	int target_stun;
	int target_dead;
	int hit_part;
	int shots;
	float age;
};

/* a hit on the target: the trace has one "# hit" line of each, and a "# shot" line for each
shot of the weapon */
#define SCENARIO_MAXIMUM_HITS 8192
#define SCENARIO_MAXIMUM_SHOTS 4096

struct scenario_shot
{
	long tick;
	short trigger;
	boolean misfired;
};

struct scenario_hit
{
	long tick;
	long part;
	float scale;
	float total;
	/* the damage effect's tag (its index among the map's tags) and how far the hit's epicentre was from the
	middle of the target */
	long damage;
	float distance;
	float origin[3];
	/* the target had already updated its own damage this tick (the hit came after it in the tick's order) */
	boolean target_updated;
};

struct scenario_input
{
	float forward;
	float strafe;
	float yaw;
	float pitch;
	boolean jump;
	boolean crouch;
	boolean fire;
	boolean melee;
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

	/* a scenario of firing: the weapon the player is given (a weapon tag's name,
	such as weapons\pistol\pistol) and the target's place */
	boolean firing;
	char weapon[96];
	boolean has_target;
	float target_x, target_y, target_z, target_yaw;
	long weapon_index;
	long target_index;
	/* the game time the target's damage last updated at */
	long target_update_time;
	/* the part of the target that was hit since the last record (-2 for a hit on no part) */
	long hit_part;
	/* the shots the weapon has fired */
	long shots;
	struct scenario_shot shot_list[SCENARIO_MAXIMUM_SHOTS];
	long hit_count;
	struct scenario_hit hits[SCENARIO_MAXIMUM_HITS];

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
		else if (!strcmp(key, "fire"))
			input->fire = value != 0.0f;
		else if (!strcmp(key, "melee"))
			input->melee = value != 0.0f;
		else if (!strcmp(key, "part"))
			;	/* (what the simulation takes the shot to hit: the engine's shot hits what it hits, which the trace says) */
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

static void scenario_harness_load(
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
		if (sscanf(line, "weapon %95[^\n]", harness.weapon) == 1)
		{
			char *slash;
			long length = (long)strlen(harness.weapon);

			/* (the name has spaces in it, and may end in some; the tags' own separator is a backslash) */
			while (length > 0 && (harness.weapon[length - 1] == ' ' || harness.weapon[length - 1] == '\r' ||
				harness.weapon[length - 1] == '\t'))
			{
				harness.weapon[--length] = 0;
			}
			for (slash = harness.weapon; *slash; slash++)
			{
				if (*slash == '/')
					*slash = '\\';
			}
			harness.firing = TRUE;
			continue;
		}
		if (sscanf(line, "target %f %f %f %f", &harness.target_x, &harness.target_y, &harness.target_z,
			&harness.target_yaw) == 4)
		{
			harness.has_target = TRUE;
			harness.firing = TRUE;
			continue;
		}
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
		int offset = 0;

		if (sscanf(line, "input %ld %ld %n", &from, &to, &offset) >= 2 && offset > 0)
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
	harness.weapon_index = NONE;
	harness.target_index = NONE;
	harness.hit_part = NONE;
	harness.target_update_time = NONE;
	if (!path[0])
		return;
	scenario_harness_load(path);
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

/* a unit has begun its own update of its damage (object_damage_update): the target's is a mark of the
order of the tick's hits: a hit that comes after it is dealt after the target's shield has ticked */
void scenario_harness_damage_update(
	long object_index)
{
	if (harness.active && harness.target_index != NONE && object_index == harness.target_index)
		harness.target_update_time = game_time_get();
}

/* a trigger of a weapon has fired (weapon_trigger_fire says so): the scenario's weapon's, for the "shots" of the
trace and the "# shot" lines (whether it misfired is the engine's random choice, which the simulation takes from here) */
void scenario_harness_shot(
	long weapon_index,
	short trigger_index,
	boolean misfired)
{
	if (!harness.active || harness.weapon_index == NONE || weapon_index != harness.weapon_index ||
		harness.first_game_tick == NONE)
	{
		return;
	}
	if (harness.shots < SCENARIO_MAXIMUM_SHOTS)
	{
		struct scenario_shot *shot = &harness.shot_list[harness.shots];

		shot->tick = game_time_get() - harness.first_game_tick;
		shot->trigger = trigger_index;
		shot->misfired = misfired;
	}
	harness.shots++;
}

/* a hit on a unit (object_cause_damage tells us of every one, once it has the
damage's total): on the target, the part of it that was hit for the trace of the
tick, and the hit itself for the "# hit" lines */
void scenario_harness_damage(
	long object_index,
	short material_index,
	struct damage_data const *damage,
	real total_damage)
{
	struct object_datum *target;
	struct scenario_hit *hit;

	if (!harness.active || harness.target_index == NONE || object_index != harness.target_index ||
		harness.first_game_tick == NONE)
	{
		return;
	}
	harness.hit_part = material_index == NONE ? -2 : material_index;
	if (harness.hit_count >= SCENARIO_MAXIMUM_HITS)
		return;
	target = (struct object_datum *)object_try_and_get_and_verify_type(object_index, _object_mask_unit);
	if (!target)
		return;
	hit = &harness.hits[harness.hit_count++];
	hit->tick = game_time_get() - harness.first_game_tick;
	hit->part = material_index;
	hit->scale = damage->scale;
	hit->total = total_damage;
	hit->damage = DATUM_INDEX_TO_ABSOLUTE_INDEX(damage->definition_index);
	{
		real dx = damage->epicenter.x - target->object.bounding_sphere_center.x;
		real dy = damage->epicenter.y - target->object.bounding_sphere_center.y;
		real dz = damage->epicenter.z - target->object.bounding_sphere_center.z;

		hit->distance = (float)sqrt(dx * dx + dy * dy + dz * dz);
	}
	hit->origin[0] = damage->origin.x;
	hit->origin[1] = damage->origin.y;
	hit->origin[2] = damage->origin.z;
	hit->target_updated = harness.target_update_time == game_time_get();
}

/* a scenario of firing: the weapon the player is given, in place of those the
game gave, and the target, once the player's unit is placed */
static void scenario_start_firing(
	long unit_index)
{
	if (harness.weapon[0])
	{
		long definition_index = tag_loaded(WEAPON_DEFINITION_TAG, harness.weapon);
		struct object_placement_data placement;
		long weapon_index;

		if (definition_index == NONE)
			scenario_fail("the game has no weapon", harness.weapon);
		object_placement_data_new(&placement, definition_index, unit_index);
		weapon_index = object_new(&placement);
		if (weapon_index == NONE || !unit_add_weapon_to_inventory(unit_index, weapon_index, _unit_add_weapon_replace))
			scenario_fail("cannot give the player the weapon", harness.weapon);
		harness.weapon_index = weapon_index;
	}
	if (harness.has_target)
	{
		struct game_globals_multiplayer_information *information = TAG_BLOCK_GET_ELEMENT(
			&scenario_get_game_globals()->multiplayer_information, 0, struct game_globals_multiplayer_information);
		struct object_placement_data placement;

		object_placement_data_new(&placement, information->unit.index, NONE);
		placement.position.x = harness.target_x;
		placement.position.y = harness.target_y;
		placement.position.z = harness.target_z;
		placement.forward.i = (real)cos(harness.target_yaw);
		placement.forward.j = (real)sin(harness.target_yaw);
		placement.forward.k = 0.0f;
		placement.up = *global_up3d;
		harness.target_index = object_new(&placement);
		if (harness.target_index == NONE)
			scenario_fail("cannot make the target", NULL);
	}
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
		if (harness.firing)
			scenario_start_firing(unit_index);
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
	if (input->fire)
		SET_FLAG(action->control_flags, _unit_control_weapon_primary_trigger_bit, TRUE);
	/* (the melee button is the engine's "use equipment" control: biped_update) */
	if (input->melee)
		SET_FLAG(action->control_flags, _unit_control_use_equipment_bit, TRUE);
	action->primary_trigger = input->fire ? 1.0f : 0.0f;
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
		scenario_fail("a tick was skipped, the trace would have a gap", harness.name);
	data_iterator_new(&iterator, player_data);
	while ((player = (struct player_datum *)data_iterator_next(&iterator)) != NULL)
	{
		if (player->local_player_index == 0 && player->unit_index != NONE)
			unit = unit_get(player->unit_index);
	}
	if (!unit)
		scenario_fail("the player's unit is gone", harness.name);

	if (tick == 10)
	{
		/* (what the game gave the player to hold, for the scenario's author) */
		short slot;

		for (slot = 0; slot < MAXIMUM_WEAPONS_PER_UNIT; slot++)
		{
			long held = unit->unit.weapon_object_indices[slot];
			struct weapon_datum *weapon = held != NONE ? weapon_try_and_get(held) : NULL;

			if (weapon)
				platform_log("scenario: the player holds %s (slot %d)", tag_get_name(weapon->definition_index), slot);
		}
	}
	aim = unit->unit.aiming_vector;
	if (!TEST_FLAG(unit->object.flags, _object_on_ground_bit))
		state |= SCENARIO_STATE_AIRBORNE;
	if (unit->unit.animation.base_seat_index == _unit_base_seat_crouch)
		state |= SCENARIO_STATE_CROUCHING;
	{
		struct scenario_record *record = &harness.records[tick];

		record->position[0] = unit->object.position.x;
		record->position[1] = unit->object.position.y;
		record->position[2] = unit->object.position.z;
		/* (world units a second, as the trace format has it) */
		record->velocity[0] = unit->object.translational_velocity.i * TICKS_PER_SECOND;
		record->velocity[1] = unit->object.translational_velocity.j * TICKS_PER_SECOND;
		record->velocity[2] = unit->object.translational_velocity.k * TICKS_PER_SECOND;
		record->yaw = (float)atan2(aim.j, aim.i);
		record->pitch = (float)asin(PIN(aim.k, -1.0f, 1.0f));
		record->state = state;
		if (harness.firing)
		{
			struct weapon_datum *weapon = harness.weapon_index != NONE ? weapon_try_and_get(harness.weapon_index) : NULL;
			struct object_datum *target = harness.target_index != NONE ?
				(struct object_datum *)object_try_and_get_and_verify_type(harness.target_index, _object_mask_unit) : NULL;

			if (weapon)
			{
				record->rounds_loaded = weapon->weapon.magazines[0].rounds_loaded;
				record->rounds_total = weapon->weapon.magazines[0].rounds_total;
				record->heat = weapon->weapon.heat;
				record->age = weapon->weapon.age;
			}
			else
			{
				record->rounds_loaded = 0;
				record->rounds_total = 0;
				record->heat = 0.0f;
			}
			if (target)
			{
				record->target_shield = target->object.shield_vitality;
				record->target_body = target->object.body_vitality;
				record->target_stun = target->object.shield_stun_ticks;
				record->target_dead = TEST_FLAG(target->object.damage_flags, _object_dead_bit) ? 1 : 0;
			}
			else
			{
				/* (a target the engine has deleted: it was dead) */
				record->target_shield = 0.0f;
				record->target_body = 0.0f;
				record->target_stun = 0;
				record->target_dead = harness.has_target ? 1 : 0;
			}
			record->hit_part = harness.hit_part;
			record->shots = (int)harness.shots;
			harness.hit_part = NONE;
		}
	}
	harness.recorded++;

	if (harness.recorded >= harness.tick_count)
	{
		FILE *file = fopen(harness.trace_path, "w");
		long index;

		if (!file)
			scenario_fail("cannot write the trace", harness.trace_path);

		fprintf(file, "# halo-trace %d\n# scenario %s\n# map %s\n# source c-engine\n", harness.firing ? 2 : 1,
			harness.name, harness.map);
		for (index = 0; index < harness.shots && index < SCENARIO_MAXIMUM_SHOTS; index++)
		{
			struct scenario_shot const *shot = &harness.shot_list[index];

			fprintf(file, "# shot %ld %d %d\n", shot->tick, (int)shot->trigger, shot->misfired ? 1 : 0);
		}
		for (index = 0; index < harness.hit_count; index++)
		{
			struct scenario_hit const *hit = &harness.hits[index];

			fprintf(file, "# hit %ld %ld %.8f %.8f %ld %.8f %.6f %.6f %.6f %d\n", hit->tick, hit->part, hit->scale,
				hit->total, hit->damage, hit->distance, hit->origin[0], hit->origin[1], hit->origin[2],
				hit->target_updated ? 1 : 0);
		}
		fprintf(file, "tick\tx\ty\tz\tvx\tvy\tvz\tyaw\tpitch\tstate%s\n",
			harness.firing ? "\trounds\ttotal\theat\tshield\tbody\tstun\tdead\thit\tshots\tage" : "");
		for (tick = 0; tick < harness.tick_count; tick++)
		{
			struct scenario_record const *record = &harness.records[tick];

			fprintf(file, "%ld\t%.6f\t%.6f\t%.6f\t%.6f\t%.6f\t%.6f\t%.6f\t%.6f\t%d", tick,
				record->position[0], record->position[1], record->position[2],
				record->velocity[0], record->velocity[1], record->velocity[2],
				record->yaw, record->pitch, record->state);
			if (harness.firing)
			{
				fprintf(file, "\t%d\t%d\t%.8f\t%.8f\t%.8f\t%d\t%d\t%d\t%d\t%.8f", record->rounds_loaded,
					record->rounds_total, record->heat, record->target_shield, record->target_body,
					record->target_stun, record->target_dead, record->hit_part, record->shots, record->age);
			}
			fprintf(file, "\n");
		}
		fclose(file);
		platform_log("scenario: %s done, %ld ticks written to %s", harness.name, harness.recorded, harness.trace_path);
		exit(EXIT_SUCCESS);
	}
}
