//! Explicit prompt context pins.
//!
//! The prompt box accepts typed mentions so the operator can force the model
//! to look at an exact file, symbol, documentation section or the current Git
//! diff. Parsing is deliberately strict and bounded: an unknown or malformed
//! mention is reported instead of being silently sent to the model.

export type MentionKind = 'file' | 'symbol' | 'docs' | 'git-diff' | 'selection';

export interface ContextMention {
  kind: MentionKind;
  /** File path, symbol name or documentation query. Empty for toggles. */
  value: string;
}

export interface ParsedMentions {
  mentions: ContextMention[];
  /** Tokens that looked like a mention but could not be resolved. */
  invalid: string[];
  /** Mentions dropped because the per-prompt cap was reached. */
  overflow: number;
}

export const MAX_CONTEXT_MENTIONS = 8;
const MAX_VALUE_CHARS = 200;

const SYMBOL_PATTERN = /^[A-Za-z_][A-Za-z0-9_:<>.,$#?!~-]{0,127}$/;
const DOCS_PATTERN = /^(?!\.\.)(?!\/)[A-Za-z0-9_.:/-]{1,120}$/;

export function mentionKey(mention: ContextMention): string {
  return `${mention.kind}:${mention.value}`;
}

export function formatMention(mention: ContextMention): string {
  switch (mention.kind) {
    case 'file':
      return `@file:${mention.value}`;
    case 'symbol':
      return `@symbol:${mention.value}`;
    case 'docs':
      return mention.value ? `@docs:${mention.value}` : '@docs';
    case 'git-diff':
      return '@git-diff';
    case 'selection':
      return '@selection';
  }
}

export function mentionLabel(mention: ContextMention): string {
  switch (mention.kind) {
    case 'file':
      return mention.value.split('/').pop() || mention.value;
    case 'symbol':
      return mention.value;
    case 'docs':
      return mention.value ? `docs: ${mention.value}` : 'project docs';
    case 'git-diff':
      return 'git diff';
    case 'selection':
      return 'selection';
  }
}

/**
 * Parses `@` mentions from a prompt. Bare `@path` is accepted only for a file
 * that exists in the current workspace list, so a typo never becomes context.
 */
export function parseMentions(
  text: string,
  knownFiles: readonly string[],
): ParsedMentions {
  const known = new Set(knownFiles);
  const mentions: ContextMention[] = [];
  const invalid: string[] = [];
  const seen = new Set<string>();
  let overflow = 0;

  const add = (mention: ContextMention): void => {
    const key = mentionKey(mention);
    if (seen.has(key)) return;
    if (mentions.length >= MAX_CONTEXT_MENTIONS) {
      overflow += 1;
      return;
    }
    seen.add(key);
    mentions.push(mention);
  };

  for (const token of text.split(/\s+/)) {
    if (!token.startsWith('@') || token.length < 2) continue;
    const body = token.slice(1);
    if (body === 'selection') {
      add({ kind: 'selection', value: '' });
      continue;
    }
    if (body === 'git-diff' || body === 'diff') {
      add({ kind: 'git-diff', value: '' });
      continue;
    }
    if (body.startsWith('file:')) {
      const path = body.slice(5);
      if (!path || path.length > MAX_VALUE_CHARS || !known.has(path)) {
        invalid.push(token);
      } else {
        add({ kind: 'file', value: path });
      }
      continue;
    }
    if (body.startsWith('symbol:')) {
      const name = body.slice(7);
      if (!SYMBOL_PATTERN.test(name)) invalid.push(token);
      else add({ kind: 'symbol', value: name });
      continue;
    }
    if (body.startsWith('docs:')) {
      const query = body.slice(5);
      if (!DOCS_PATTERN.test(query) || query.includes('..')) invalid.push(token);
      else add({ kind: 'docs', value: query });
      continue;
    }
    if (body === 'docs') {
      add({ kind: 'docs', value: '' });
      continue;
    }
    if (known.has(body)) {
      add({ kind: 'file', value: body });
    } else {
      invalid.push(token);
    }
  }

  return { mentions, invalid, overflow };
}

/** Adds a mention to an existing set, returning a new bounded array. */
export function addMention(
  current: readonly ContextMention[],
  mention: ContextMention,
): ContextMention[] {
  const key = mentionKey(mention);
  if (current.some((entry) => mentionKey(entry) === key)) return [...current];
  if (current.length >= MAX_CONTEXT_MENTIONS) return [...current];
  return [...current, mention];
}
