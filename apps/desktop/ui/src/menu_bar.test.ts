import { describe, expect, it } from 'vitest';
import { COMMAND_ID_PATTERN } from './commands';
import { MENUS, menuCommandIds } from './menu_bar';

describe('menus', () => {
  it('has the standard top-level menus in order', () => {
    expect(MENUS.map((menu) => menu.label)).toEqual(['File', 'Edit', 'View', 'Run', 'Help']);
  });
  it('references well-formed command ids, each at most once', () => {
    const ids = menuCommandIds();
    expect(ids.every((id) => COMMAND_ID_PATTERN.test(id))).toBe(true);
    expect(new Set(ids).size).toBe(ids.length);
  });
});
