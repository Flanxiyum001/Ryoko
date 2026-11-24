import { describe, expect, it } from 'vitest';
import { firstCodeFence, mentionedFiles } from './agent_chat';

describe('agent context parser', () => {
  it('attaches only existing workspace paths and deduplicates', () => {
    expect(mentionedFiles('Inspect @src/lib.rs @src/lib.rs and @bad.rs', ['src/lib.rs']))
      .toEqual(['src/lib.rs']);
    expect(mentionedFiles('Use @selection', ['src/lib.rs'])).toEqual([]);
  });
  it('refuses unresolved mention and limits attachments', () => {
    const known = ['a.ts', 'b.ts', 'c.ts', 'd.ts', 'e.ts'];
    expect(mentionedFiles(known.map((path) => '@' + path).join(' '), known))
      .toEqual(known);
  });
  it('extracts exactly the first fenced replacement, not prose', () => {
    expect(firstCodeFence('Here is a change:\n```ts\nconst x = 1;\n```\nDone'))
      .toBe('const x = 1;');
    expect(firstCodeFence('No code provided')).toBeNull();
  });
});
