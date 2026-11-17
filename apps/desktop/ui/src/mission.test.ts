import { describe, expect, it } from 'vitest';
import {
  focusMissionTasks,
  formatLedgerCost,
  formatTokens,
  layoutMission,
  liveStatus,
  parseMissionLog,
  reduceMissionEvents,
  roleLabel,
  type MissionTask,
} from './mission';

const log = [
  '{"type":"run_started","run_id":"run-1","task_count":2}',
  '{"type":"task_scheduled","task_id":"a","role":"project_manager","description":"Plan","dependencies":[]}',
  '{"type":"task_scheduled","task_id":"b","role":"senior_developer","description":"Build","dependencies":["a"]}',
  '{"type":"task_started","task_id":"a","role":"project_manager","agent":"pm-1","attempt":1}',
  '{"type":"task_succeeded","task_id":"a","role":"project_manager","output":"planned","attempts":1}',
  '{"type":"task_started","task_id":"b","role":"senior_developer","agent":"dev-1","attempt":1}',
  '{"type":"task_failed","task_id":"b","role":"senior_developer","error":"tests failed","attempts":1,"retrying":true}',
  '{"type":"task_started","task_id":"b","role":"senior_developer","agent":"dev-1","attempt":2}',
  '{"type":"validation_finished","task_id":"b","passed":true,"summary":"cargo test","logs":[],"duration_ms":12}',
  '{"type":"task_succeeded","task_id":"b","role":"senior_developer","output":"done","attempts":2}',
  '{"type":"run_finished","succeeded":2,"failed":0,"skipped":0,"duration_ms":40}',
].join('\n');

describe('mission event log', () => {
  it('parses JSONL and ignores partial trailing lines', () => {
    const events = parseMissionLog(`${log}\n{"type":"task_st`);
    expect(events).toHaveLength(11);
    expect(events[0].type).toBe('run_started');
  });

  it('reduces retries, validation and outputs into task state', () => {
    const snapshot = reduceMissionEvents(parseMissionLog(log));
    expect(snapshot.runId).toBe('run-1');
    expect(snapshot.taskCount).toBe(2);
    expect(snapshot.tasks.map((task) => task.id)).toEqual(['a', 'b']);
    const developer = snapshot.tasks[1];
    expect(developer.status).toBe('succeeded');
    expect(developer.attempts).toBe(2);
    expect(developer.output).toBe('done');
    expect(developer.validation?.passed).toBe(true);
    expect(snapshot.finished?.succeeded).toBe(2);
  });

  it('tracks per-task token usage and run totals', () => {
    const usage = '{"type":"task_usage","task_id":"b","requests":2,"input_tokens":420,"output_tokens":125,"ledger_cost_usd":0.03}';
    const snapshot = reduceMissionEvents(parseMissionLog([log, usage].join(String.fromCharCode(10))));
    expect(snapshot.tasks[1].usage).toEqual({
      requests: 2,
      inputTokens: 420,
      outputTokens: 125,
      ledgerCostUsd: 0.03,
    });
    expect(snapshot.totals).toEqual({
      requests: 2,
      inputTokens: 420,
      outputTokens: 125,
      ledgerCostUsd: 0.03,
    });
    expect(formatTokens(1_240)).toBe('1.2k');
    expect(formatLedgerCost(0.03)).toBe('$0.0300');
  });

  it('tracks live phases and a timestamped activity timeline', () => {
    const records = [
      '{"type":"task_scheduled","task_id":"x","role":"backend","description":"Build","dependencies":[],"at_ms":1000}',
      '{"type":"task_started","task_id":"x","agent":"backend","attempt":1,"at_ms":1100}',
      '{"type":"task_phase","task_id":"x","phase":"thinking","at_ms":1150}',
      '{"type":"task_phase","task_id":"x","phase":"coding","at_ms":2200}',
      '{"type":"task_progress","task_id":"x","message":"wrote src/lib.rs","at_ms":2300}',
      '{"type":"validation_started","task_id":"x","file_count":1,"at_ms":3000}',
      '{"type":"validation_finished","task_id":"x","passed":false,"summary":"cargo test failed","logs":["E1"],"duration_ms":12,"at_ms":4000}',
      '{"type":"task_failed","task_id":"x","stage":"validation","error":"cargo test failed","attempts":1,"retrying":false,"at_ms":4050}',
    ].join('\n');
    const task = reduceMissionEvents(parseMissionLog(records)).tasks[0];
    expect(task.phase).toBe('testing');
    expect(task.failureStage).toBe('validation');
    expect(liveStatus(task)).toBe('Failed');
    expect(task.activity?.map((entry) => entry.label)).toEqual([
      'thinking',
      'coding',
      'Progress',
      'Testing',
      'Validation failed',
      'Failed',
    ]);
    expect(task.activity?.[1].atMs).toBe(2200);
  });

  it('reports waiting, thinking, coding and testing live states', () => {
    const base: MissionTask = {
      id: 't',
      role: 'backend',
      description: 'Build',
      dependencies: [],
      status: 'scheduled',
    };
    expect(liveStatus(base)).toBe('Waiting');
    expect(liveStatus({ ...base, status: 'running' })).toBe('Thinking');
    expect(liveStatus({ ...base, status: 'running', phase: 'coding' })).toBe('Coding');
    expect(liveStatus({ ...base, status: 'running', phase: 'testing' })).toBe('Testing');
    expect(liveStatus({ ...base, status: 'validating' })).toBe('Testing');
    expect(liveStatus({ ...base, status: 'succeeded' })).toBe('Succeeded');
  });

  it('retains validation and failure logs across retries without overwriting history', () => {
    const records = [
      '{"type":"task_started","task_id":"x","agent":"backend","attempt":1}',
      '{"type":"validation_finished","task_id":"x","passed":false,"summary":"compile failed","logs":["error A"],"duration_ms":10}',
      '{"type":"task_failed","task_id":"x","attempts":1,"retrying":true,"error":"compile failed"}',
      '{"type":"task_started","task_id":"x","agent":"backend","attempt":2}',
      '{"type":"validation_finished","task_id":"x","passed":true,"summary":"passed","logs":["ok"],"duration_ms":15}',
      '{"type":"task_succeeded","task_id":"x","attempts":2,"output":"fixed"}',
    ].join('\n');
    const task = reduceMissionEvents(parseMissionLog(records)).tasks[0];
    expect(task.history?.map((entry) => entry.status)).toEqual(['retrying', 'succeeded']);
    expect(task.history?.[0].validation?.logs).toEqual(['error A']);
    expect(task.history?.[1].validation?.summary).toBe('passed');
    expect(task.history?.[1].output).toBe('fixed');
  });

  it('keeps skipped dependency tasks free of fabricated attempts', () => {
    const task = reduceMissionEvents([{ type: 'task_skipped', task_id: 'blocked', reason: 'dependency failed' }]).tasks[0];
    expect(task.history).toBeUndefined();
    expect(task.status).toBe('skipped');
  });

  it('labels built-in and custom roles', () => {
    expect(roleLabel('qa_engineer')).toBe('qa-engineer');
    expect(roleLabel({ custom: 'security' })).toBe('security');
    expect(roleLabel(undefined)).toBe('agent');
  });
});

describe('mission layout', () => {
  const tasks: MissionTask[] = [
    {
      id: 'a',
      role: 'project-manager',
      description: 'Plan',
      dependencies: [],
      status: 'succeeded',
    },
    {
      id: 'b',
      role: 'senior-developer',
      description: 'Build',
      dependencies: ['a'],
      status: 'running',
    },
    {
      id: 'c',
      role: 'qa-engineer',
      description: 'Test',
      dependencies: ['b'],
      status: 'scheduled',
    },
  ];

  it('places dependencies in earlier columns', () => {
    const layout = layoutMission(tasks);
    const positions = new Map(layout.nodes.map((node) => [node.task.id, node]));
    expect(positions.get('a')?.x).toBeLessThan(positions.get('b')!.x);
    expect(positions.get('b')?.x).toBeLessThan(positions.get('c')!.x);
    expect(layout.edges).toHaveLength(2);
  });

  it('focuses on dependencies and immediate consumers without losing their edges', () => {
    const filtered = focusMissionTasks([
      ...tasks,
      { id: 'unrelated', role: 'tester', description: 'Unrelated', dependencies: [], status: 'running' },
    ], 'b');
    expect(filtered.map((task) => task.id)).toEqual(['a', 'b', 'c']);
    expect(layoutMission(filtered).edges).toHaveLength(2);
    expect(focusMissionTasks(tasks, null)).toBe(tasks);
  });

  it('does not hang on cyclic input', () => {
    const cyclic = tasks.map((task) => ({
      ...task,
      dependencies: task.id === 'a' ? ['c'] : task.dependencies,
    }));
    expect(layoutMission(cyclic).nodes).toHaveLength(3);
  });
});
