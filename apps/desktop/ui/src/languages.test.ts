import { describe, expect, it } from 'vitest';
import { LANGUAGE_IDS, defaultRunProfile, detectLanguage, parseTaskArgs } from './languages';

describe('polyglot detection', () => {
  it('recognizes case-insensitive extensions and special filenames', () => {
    expect(detectLanguage('src/Main.RS')).toBe('rust');
    expect(detectLanguage('C:\\code\\Dockerfile')).toBe('dockerfile');
    expect(detectLanguage('Makefile')).toBe('makefile');
    expect(detectLanguage('README')).toBe('markdown');
    expect(detectLanguage('main.tf')).toBe('hcl');
    expect(detectLanguage('script.ps1')).toBe('powershell');
    expect(detectLanguage('schema.graphql')).toBe('graphql');
    expect(detectLanguage('main.zig')).toBe('zig');
  });
  it('detects shebangs but never guesses unknown file types', () => {
    expect(detectLanguage('tools/test', '#!/usr/bin/env python3\n')).toBe('python');
    expect(detectLanguage('bin/entry', '#!/bin/sh\n')).toBe('shell');
    expect(detectLanguage('script.foo', '#!/usr/bin/env ruby\n')).toBe('ruby');
    expect(detectLanguage('mystery.foo', 'hello')).toBe('plaintext');
  });
  it('has unique Monaco/LSP ids', () => {
    expect(LANGUAGE_IDS).toContain('typescript');
    expect(LANGUAGE_IDS).toContain('cpp');
    expect(LANGUAGE_IDS.length).toBe(new Set(LANGUAGE_IDS).size);
  });
});
describe('run profiles', () => {
  it('returns literal argv with file placeholders', () => {
    expect(defaultRunProfile('src/main.py')).toEqual({
      label:'Python', program:'python3', args:['-u','{file}'],
    });
    expect(defaultRunProfile('main.go')?.program).toBe('go');
    expect(defaultRunProfile('main.rs')?.args).toEqual(['run','--locked']);
  });
  it('does not pretend JSX, TSX, HTML or unknown files are executable', () => {
    expect(defaultRunProfile('web/App.tsx')).toBeNull();
    expect(defaultRunProfile('web/App.jsx')).toBeNull();
    expect(defaultRunProfile('web/index.html')).toBeNull();
    expect(defaultRunProfile('code.nope')).toBeNull();
  });
});
describe('literal argv parsing', () => {
  it('groups quotes and escaped spaces', () => {
    expect(parseTaskArgs('--flag "hello world" name\\ with\\ spaces'))
      .toEqual(['--flag', 'hello world', 'name with spaces']);
  });
  it('preserves unescaped backslashes and empty tokens', () => {
    expect(parseTaskArgs('--path C:\\test\\bin ""'))
      .toEqual(['--path', 'C:\\test\\bin', '']);
  });
  it('rejects incomplete quotes', () => {
    expect(() => parseTaskArgs('"missing')).toThrow(/Close/);
  });
});
