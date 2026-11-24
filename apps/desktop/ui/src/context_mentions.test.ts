import { describe, expect, it } from 'vitest';
import {
  MAX_CONTEXT_MENTIONS,
  addMention,
  formatMention,
  mentionLabel,
  parseMentions,
} from './context_mentions';

const files = ['src/main.rs', 'docs/guide.md', 'src/context.rs'];

describe('parseMentions', () => {
  it('resolves bare paths only when the file is known', () => {
    const parsed = parseMentions('look at @src/main.rs and @missing.rs', files);
    expect(parsed.mentions).toEqual([{ kind: 'file', value: 'src/main.rs' }]);
    expect(parsed.invalid).toEqual(['@missing.rs']);
  });

  it('parses typed file, symbol and docs mentions', () => {
    const parsed = parseMentions(
      '@file:src/context.rs @symbol:WorkspaceIndex @docs:architecture @git-diff @selection',
      files,
    );
    expect(parsed.mentions.map(formatMention)).toEqual([
      '@file:src/context.rs',
      '@symbol:WorkspaceIndex',
      '@docs:architecture',
      '@git-diff',
      '@selection',
    ]);
    expect(parsed.invalid).toEqual([]);
  });

  it('rejects unknown files, malformed symbols and empty docs queries', () => {
    const parsed = parseMentions(
      '@file:src/secret.rs @symbol:9lives @docs: @file: @docs:../secret',
      files,
    );
    expect(parsed.mentions).toEqual([]);
    expect(parsed.invalid).toEqual([
      '@file:src/secret.rs',
      '@symbol:9lives',
      '@docs:',
      '@file:',
      '@docs:../secret',
    ]);
  });

  it('deduplicates mentions and caps the prompt', () => {
    const text = Array.from({ length: MAX_CONTEXT_MENTIONS + 3 }, (_, index) => `@symbol:s${index}`).join(' ');
    const parsed = parseMentions(`@symbol:s0 ${text} @symbol:s0`, files);
    expect(parsed.mentions).toHaveLength(MAX_CONTEXT_MENTIONS);
    expect(parsed.overflow).toBe(3);
  });

  it('ignores email addresses and ordinary text', () => {
    const parsed = parseMentions('mail dev@example.com about main.rs', files);
    expect(parsed.mentions).toEqual([]);
    expect(parsed.invalid).toEqual([]);
  });
});

describe('addMention', () => {
  it('adds once and never exceeds the cap', () => {
    let mentions = addMention([], { kind: 'symbol', value: 'run' });
    mentions = addMention(mentions, { kind: 'symbol', value: 'run' });
    expect(mentions).toHaveLength(1);
    for (let index = 0; index < MAX_CONTEXT_MENTIONS + 2; index += 1) {
      mentions = addMention(mentions, { kind: 'symbol', value: `s${index}` });
    }
    expect(mentions).toHaveLength(MAX_CONTEXT_MENTIONS);
  });
});

describe('labels', () => {
  it('shows a short file label and a docs label', () => {
    expect(mentionLabel({ kind: 'file', value: 'src/context.rs' })).toBe('context.rs');
    expect(mentionLabel({ kind: 'docs', value: 'security' })).toBe('docs: security');
    expect(mentionLabel({ kind: 'docs', value: '' })).toBe('project docs');
  });
});
