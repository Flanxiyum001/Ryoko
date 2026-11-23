//! Workbench command registry: one table of named actions shared by the
//! command palette, the in-window menu bar, the native macOS menu and
//! keybindings. Pure logic lives here so ranking and lookup are testable.

export interface WorkbenchCommand {
  /** Stable identifier, e.g. `workbench.quickOpen`. */
  id: string;
  /** Category shown before the title in the palette, e.g. `View`. */
  category: string;
  title: string;
  run: () => void | Promise<void>;
  /** When present and false, the command is listed but cannot run. */
  enabled?: () => boolean;
}

export interface CommandMatch {
  command: WorkbenchCommand;
  score: number;
  /** Indexes in `label(command)` that matched the query, for highlighting. */
  positions: number[];
}

export const COMMAND_ID_PATTERN = /^[a-z][a-zA-Z]*(\.[a-z][a-zA-Z0-9]*)+$/;

export function commandLabel(command: Pick<WorkbenchCommand, 'category' | 'title'>): string {
  return `${command.category}: ${command.title}`;
}

/**
 * Subsequence fuzzy match. Every query character must appear in order.
 * Consecutive runs and word starts score higher; shorter labels win ties.
 * Returns null when the query does not match.
 */
export function fuzzyScore(label: string, rawQuery: string): { score: number; positions: number[] } | null {
  const query = rawQuery.trim().toLowerCase().replace(/\s+/g, ' ');
  if (!query) return { score: 0, positions: [] };
  const text = label.toLowerCase();
  const positions: number[] = [];
  let score = 0;
  let from = 0;
  let previous = -2;
  for (const char of query) {
    if (char === ' ') continue;
    const index = text.indexOf(char, from);
    if (index < 0) return null;
    const wordStart = index === 0 || /[\s:./_-]/.test(text[index - 1]);
    score += 1;
    if (index === previous + 1) score += 5;
    if (wordStart) score += 8;
    positions.push(index);
    previous = index;
    from = index + 1;
  }
  if (text.includes(query)) score += 20;
  score -= label.length * 0.05;
  return { score, positions };
}

/** Ranks commands for a palette query; recently used commands lead an empty query. */
export function rankCommands(
  commands: readonly WorkbenchCommand[],
  query: string,
  recent: readonly string[] = [],
): CommandMatch[] {
  const recentRank = new Map(recent.map((id, index) => [id, index]));
  const matches: CommandMatch[] = [];
  for (const command of commands) {
    const result = fuzzyScore(commandLabel(command), query);
    if (!result) continue;
    const bonus = recentRank.has(command.id) ? 30 - Math.min(recentRank.get(command.id)!, 29) : 0;
    matches.push({ command, score: result.score + (query.trim() ? bonus / 10 : bonus), positions: result.positions });
  }
  return matches.sort((left, right) =>
    right.score - left.score || commandLabel(left.command).localeCompare(commandLabel(right.command)));
}

export class CommandRegistry {
  private readonly commands = new Map<string, WorkbenchCommand>();

  register(command: WorkbenchCommand): void {
    if (!COMMAND_ID_PATTERN.test(command.id)) throw new Error(`Invalid command id: ${command.id}`);
    if (this.commands.has(command.id)) throw new Error(`Duplicate command id: ${command.id}`);
    this.commands.set(command.id, command);
  }

  get(id: string): WorkbenchCommand | undefined {
    return this.commands.get(id);
  }

  has(id: string): boolean {
    return this.commands.has(id);
  }

  all(): WorkbenchCommand[] {
    return [...this.commands.values()];
  }

  isEnabled(id: string): boolean {
    const command = this.commands.get(id);
    return !!command && (command.enabled?.() ?? true);
  }

  /**
   * Runs a command by id. Unknown ids (for example from a native menu event)
   * are rejected rather than interpreted, so only registered actions run.
   */
  async execute(id: unknown): Promise<boolean> {
    if (typeof id !== 'string') return false;
    const command = this.commands.get(id);
    if (!command || !(command.enabled?.() ?? true)) return false;
    await command.run();
    return true;
  }
}

/** Renders `MOD+SHIFT+P` as `Ctrl+Shift+P` or `⇧⌘P` for display. */
export function formatChord(chord: string, mac: boolean): string {
  const parts = chord.split('+').filter(Boolean);
  const key = parts.pop() ?? '';
  const names: Record<string, [string, string]> = {
    MOD: ['Ctrl', '⌘'],
    SHIFT: ['Shift', '⇧'],
    ALT: ['Alt', '⌥'],
  };
  const keyName = key.length === 1 ? key.toUpperCase() : key.charAt(0) + key.slice(1).toLowerCase();
  if (mac) {
    const order = ['ALT', 'SHIFT', 'MOD'];
    const mods = order.filter((mod) => parts.includes(mod)).map((mod) => names[mod][1]);
    return mods.join('') + keyName;
  }
  return [...parts.map((mod) => names[mod]?.[0] ?? mod), keyName].join('+');
}
