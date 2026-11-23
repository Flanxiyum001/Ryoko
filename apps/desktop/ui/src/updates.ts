//! Update checks: when to check, when to tell the user, and how to describe
//! progress. The renderer only asks Rust to check or install; nothing is
//! downloaded until the user chooses "Download and install".

export const UPDATE_PREFS_STORAGE = 'ryoko.updates.v1';
/** Automatic checks run at most this often. */
export const AUTO_CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000;

export interface UpdatePrefs {
  autoCheck: boolean;
  /** A version the user chose to skip; it is not announced again. */
  skippedVersion: string | null;
  lastCheck: number;
}

export interface UpdateInfo {
  version: string;
  currentVersion: string;
  notes: string;
  date: string | null;
}

export interface UpdateStatus {
  configured: boolean;
  currentVersion: string;
}

export const DEFAULT_UPDATE_PREFS: Readonly<UpdatePrefs> = { autoCheck: true, skippedVersion: null, lastCheck: 0 };

export function sanitizePrefs(raw: unknown): UpdatePrefs {
  const value = (typeof raw === 'object' && raw !== null ? raw : {}) as Record<string, unknown>;
  return {
    autoCheck: typeof value.autoCheck === 'boolean' ? value.autoCheck : DEFAULT_UPDATE_PREFS.autoCheck,
    skippedVersion: typeof value.skippedVersion === 'string' && value.skippedVersion.length <= 64 ? value.skippedVersion : null,
    lastCheck: typeof value.lastCheck === 'number' && Number.isFinite(value.lastCheck) && value.lastCheck >= 0 ? value.lastCheck : 0,
  };
}

export function loadPrefs(storage: Pick<Storage, 'getItem'>): UpdatePrefs {
  try {
    return sanitizePrefs(JSON.parse(storage.getItem(UPDATE_PREFS_STORAGE) ?? '{}'));
  } catch {
    return { ...DEFAULT_UPDATE_PREFS };
  }
}

export function savePrefs(storage: Pick<Storage, 'setItem'>, prefs: UpdatePrefs): void {
  try {
    storage.setItem(UPDATE_PREFS_STORAGE, JSON.stringify(sanitizePrefs(prefs)));
  } catch {
    // Storage is optional.
  }
}

export function shouldAutoCheck(prefs: UpdatePrefs, now: number): boolean {
  // A clock moved backwards also allows a check.
  return prefs.autoCheck && (now - prefs.lastCheck >= AUTO_CHECK_INTERVAL_MS || now < prefs.lastCheck);
}

/** Automatic checks respect "Skip this version"; a manual check always shows the update. */
export function shouldAnnounce(info: UpdateInfo | null, prefs: UpdatePrefs, manual: boolean): boolean {
  if (!info) return false;
  return manual || info.version !== prefs.skippedVersion;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024 * 1024) return `${Math.max(0, Math.round(bytes / 1024))} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function progressLabel(downloaded: number, total: number | null): string {
  if (total && total > 0) {
    const percent = Math.min(100, Math.floor((downloaded / total) * 100));
    return `Downloading update… ${percent}% (${formatBytes(downloaded)} of ${formatBytes(total)})`;
  }
  return `Downloading update… ${formatBytes(downloaded)}`;
}
