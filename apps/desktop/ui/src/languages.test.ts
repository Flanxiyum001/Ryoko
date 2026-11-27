import { describe, expect, it } from 'vitest';
import { LANGUAGE_IDS, defaultRunProfile, detectEditorLanguage, detectLanguage, parseTaskArgs } from './languages';

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
  it('recognizes additional project languages, extensionless config, and BOM scripts', () => {
    expect(detectLanguage('main.mts')).toBe('typescript');
    expect(detectLanguage('App.svelte')).toBe('svelte');
    expect(detectLanguage('main.bicep')).toBe('bicep');
    expect(detectLanguage('.env.local')).toBe('ini');
    expect(detectLanguage('Vagrantfile')).toBe('ruby');
    expect(detectLanguage('script', '\uFEFF#!/usr/bin/env python3')).toBe('python');
    expect(detectLanguage('sim.f90')).toBe('fortran');
    expect(detectLanguage('server.erl')).toBe('erlang');
    expect(detectLanguage('main.odin')).toBe('odin');
    expect(detectLanguage('unit.pas')).toBe('pascal');
    expect(detectLanguage('core.sv')).toBe('systemverilog');
    expect(detectLanguage('shader.wgsl')).toBe('wgsl');
    expect(detectLanguage('App.csproj')).toBe('xml');
    expect(detectLanguage('events.jsonl')).toBe('json');
    expect(detectLanguage('.gitconfig')).toBe('ini');
    expect(detectLanguage('tool', '#!/usr/bin/env escript\n')).toBe('erlang');
  });
  it('uses registered editor modes and conservative content detection', () => {
    const modes = [{ id: 'custom', extensions: ['.abc'], filenames: ['CUSTOMFILE'] },
      { id: 'compound', extensions: ['.test.abc'] }];
    expect(detectEditorLanguage('test.ABC', '', modes)).toBe('custom');
    expect(detectEditorLanguage('x.test.abc', '', modes)).toBe('compound');
    expect(detectEditorLanguage('CUSTOMFILE', '', modes)).toBe('custom');
    expect(detectEditorLanguage('data', '{"x": 1}', [])).toBe('json');
    expect(detectEditorLanguage('document', '<?xml version="1.0"?>', [])).toBe('xml');
    expect(detectEditorLanguage('notes.txt', '{"x": 1}', [])).toBe('plaintext');
    expect(detectEditorLanguage('unknown', 'arbitrary words', [])).toBe('plaintext');
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
