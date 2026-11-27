import { describe, expect, it } from 'vitest';
import { changeStats, toolLabel } from './agent_harness';

describe('agent harness view helpers', () => {
  it('labels tools in plain language', () => {
    expect(toolLabel('edit_file')).toBe('Edited a file');
    expect(toolLabel('github_search')).toBe('Used github_search');
  });

  it('counts added and removed lines', () => {
    expect(changeStats({ path: 'a', original: 'a\nb\nc', content: 'a\nB\nc\nd' }))
      .toEqual({ added: 2, removed: 1 });
    expect(changeStats({ path: 'n', original: null, content: 'x\ny' }))
      .toEqual({ added: 2, removed: 0 });
  });
});
