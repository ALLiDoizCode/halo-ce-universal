// The gate, run DETERMINISTICALLY by the runner, not asked of the agent.
//
// implement-prompt.md tells the agent to run the gate before it commits, but that is
// advisory prose the agent reports on itself. Verifying a build is plumbing, so the
// runner does it, and never opens a PR while it is red.
//
// THE RULE (the one place it lives; CLAUDE.md points here): the gate is whatever
// `.github/workflows/ci.yml` on the base branch says it is. `gateFromCi` takes that
// file's text and returns the `run:` steps of its gate job, in order:
//
//   - the gate job is the job named `gate`, else the job named `checks`;
//   - `uses:` steps (checkout, toolchain setup, caches) are not commands and are left
//     out, because the sandbox image is its own environment;
//   - a step this runner cannot run faithfully (an `if:` condition, or a `${{ }}`
//     expression) is skipped, and the skip is reported, never silent;
//   - `working-directory:` and `env:` on a step are applied to it.
//
// When there is no ci.yml, or no gate job in it, or no runnable step, THE GATE RUNS
// NOTHING and says so in the log. That is the honest state of a repository with no CI,
// and it means the first CI job that lands is gated on with no further factory change.
// A ci.yml that is not valid YAML is an error: a broken file must not read as "no gate".
//
// The file is read from the BASE branch (see `loadGate`), not the branch under test, so
// an agent cannot edit its own gate.

import { parse } from 'yaml';

import type * as sandcastle from '@ai-hero/sandcastle';

type Sandbox = Awaited<ReturnType<typeof sandcastle.createSandbox>>;

export interface GateStep {
  readonly name: string;
  readonly command: string;
}

export interface GateFailure {
  readonly step: string;
  readonly command: string;
  readonly exitCode: number;
  /** Tail of combined output — enough for an agent to act on, bounded so it cannot blow a prompt. */
  readonly output: string;
}

export interface GateResult {
  readonly passed: boolean;
  readonly ran: readonly string[];
  readonly failure: GateFailure | null;
}

/** Keep fed-back output useful but bounded — a full build log is megabytes. */
const MAX_OUTPUT_CHARS = 12_000;

export interface Gate {
  readonly steps: readonly GateStep[];
  /** What was skipped or missing, one line each. Printed at the top of the gate's log. */
  readonly notes: readonly string[];
}

const GATE_JOBS = ['gate', 'checks'] as const;

const shellQuote = (value: string): string => `'${value.replace(/'/g, `'\\''`)}'`;

interface CiStep {
  name?: unknown;
  run?: unknown;
  uses?: unknown;
  if?: unknown;
  env?: unknown;
  'working-directory'?: unknown;
}

/** The gate, from the text of ci.yml (`null` when there is no such file). */
export function gateFromCi(ciYaml: string | null): Gate {
  const none = (why: string): Gate => ({
    steps: [],
    notes: [`${why} The gate runs NOTHING, so a green gate here proves nothing.`],
  });

  if (ciYaml === null) return none('There is no .github/workflows/ci.yml on the base branch.');

  const doc = parse(ciYaml) as { jobs?: Record<string, { steps?: CiStep[] }> } | null;
  const jobs = doc?.jobs ?? {};
  const jobName = GATE_JOBS.find((name) => name in jobs);
  if (!jobName) {
    return none('ci.yml has no `gate` or `checks` job.');
  }

  const steps: GateStep[] = [];
  const notes: string[] = [];
  (jobs[jobName]?.steps ?? []).forEach((step, index) => {
    if (typeof step.run !== 'string') return; // `uses:` — setup, not a command
    const name = typeof step.name === 'string' ? step.name : `step ${index + 1}`;
    if (step.if !== undefined) {
      notes.push(`Skipped "${name}": it has an \`if:\` condition this runner cannot evaluate.`);
      return;
    }
    if (JSON.stringify(step).includes('${{')) {
      notes.push(`Skipped "${name}": it uses a \${{ }} expression this runner cannot evaluate.`);
      return;
    }
    let command = step.run.replace(/\n$/, '');
    const env = Object.entries((step.env ?? {}) as Record<string, unknown>);
    if (env.length > 0) {
      command = `export ${env.map(([k, v]) => `${k}=${shellQuote(String(v))}`).join(' ')} && ${command}`;
    }
    if (typeof step['working-directory'] === 'string') {
      command = `cd ${shellQuote(step['working-directory'])} && ${command}`;
    }
    steps.push({ name, command });
  });

  if (steps.length === 0) {
    notes.push(`The \`${jobName}\` job in ci.yml has no runnable step. The gate runs NOTHING.`);
  }
  return { steps, notes };
}

/**
 * The gate for the branch under test: ci.yml as it is on `base`. Logs every note, and
 * says loudly when the gate is empty.
 */
export async function loadGate(sandbox: Sandbox, base: string): Promise<Gate> {
  const shown = await sandbox.exec(`git show ${base}:.github/workflows/ci.yml`);
  const gate = gateFromCi(shown.exitCode === 0 ? shown.stdout : null);
  for (const note of gate.notes) console.log(`  [gate] NOTE: ${note}`);
  return gate;
}

/**
 * Run `steps` in order, stopping at the first failure.
 *
 * Failure is returned, not thrown, so the caller can decide between a fix
 * iteration and failing the job.
 */
export async function runGate(sandbox: Sandbox, steps: readonly GateStep[]): Promise<GateResult> {
  const ran: string[] = [];

  for (const step of steps) {
    console.log(`  [gate] ${step.name}: ${step.command}`);
    const lines: string[] = [];
    const result = await sandbox.exec(`set -eo pipefail\n${step.command}`, {
      onLine: (line) => {
        lines.push(line);
        // Stream sparingly: full build output would bury the runner log.
        if (lines.length <= 40) console.log(`    | ${line}`);
      },
    });
    ran.push(step.name);

    if (result.exitCode !== 0) {
      const combined = [result.stdout, result.stderr].filter(Boolean).join('\n');
      const output =
        combined.length > MAX_OUTPUT_CHARS
          ? `...(truncated to the last ${MAX_OUTPUT_CHARS} chars)...\n` +
            combined.slice(-MAX_OUTPUT_CHARS)
          : combined;

      console.log(`  [gate] FAILED at ${step.name} (exit ${result.exitCode}).`);
      return {
        passed: false,
        ran,
        failure: { step: step.name, command: step.command, exitCode: result.exitCode, output },
      };
    }
  }

  if (ran.length === 0) console.log('  [gate] NOTHING RAN: there is no gate to pass.');
  else console.log(`  [gate] PASSED (${ran.length} step(s): ${ran.join(', ')}).`);
  return { passed: true, ran, failure: null };
}

/** The prompt handed to a fix iteration. Concrete failure, no room to reinterpret the task. */
export function fixPrompt(failure: GateFailure, attempt: number, maxAttempts: number): string {
  return [
    `The repository gate is RED. This is fix attempt ${attempt} of ${maxAttempts}.`,
    '',
    `Failing step: ${failure.step}`,
    `Command:      ${failure.command}`,
    `Exit code:    ${failure.exitCode}`,
    '',
    'Output:',
    '```',
    failure.output,
    '```',
    '',
    'Fix the cause and commit. Rules:',
    `- Re-run \`${failure.command}\` yourself and confirm it passes before you finish.`,
    '- Fix the code. Do NOT weaken, skip, delete or ignore a test, and do not',
    '  loosen a lint to make this pass — if the test is genuinely wrong, say so',
    '  explicitly in the commit message and explain why.',
    '- Change only what this failure requires. Do not refactor beyond it.',
    '- If you cannot fix it, commit nothing and explain what is blocking you.',
  ].join('\n');
}
