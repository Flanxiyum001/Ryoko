//! Vendored Lucide SVG geometry (0.468.0), selected from the 21st.dev icon catalog.
//! See ui/public/licenses/lucide.txt. Only trusted, static geometry is used.

const SHAPES = {
  'folder': '<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />',
  'folder-open': '<path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2" />',
  'file': '<path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" /> <path d="M14 2v4a2 2 0 0 0 2 2h4" />',
  'save': '<path d="M15.2 3a2 2 0 0 1 1.4.6l3.8 3.8a2 2 0 0 1 .6 1.4V19a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z" /> <path d="M17 21v-7a1 1 0 0 0-1-1H8a1 1 0 0 0-1 1v7" /> <path d="M7 3v4a1 1 0 0 0 1 1h7" />',
  'play': '<polygon points="6 3 20 12 6 21 6 3" />',
  'square': '<rect width="18" height="18" x="3" y="3" rx="2" />',
  'terminal': '<polyline points="4 17 10 11 4 5" /> <line x1="12" x2="20" y1="19" y2="19" />',
  'bug': '<path d="m8 2 1.88 1.88" /> <path d="M14.12 3.88 16 2" /> <path d="M9 7.13v-1a3.003 3.003 0 1 1 6 0v1" /> <path d="M12 20c-3.3 0-6-2.7-6-6v-3a4 4 0 0 1 4-4h4a4 4 0 0 1 4 4v3c0 3.3-2.7 6-6 6" /> <path d="M12 20v-9" /> <path d="M6.53 9C4.6 8.8 3 7.1 3 5" /> <path d="M6 13H2" /> <path d="M3 21c0-2.1 1.7-3.9 3.8-4" /> <path d="M20.97 5c0 2.1-1.6 3.8-3.5 4" /> <path d="M22 13h-4" /> <path d="M17.2 17c2.1.1 3.8 1.9 3.8 4" />',
  'send': '<path d="M14.536 21.686a.5.5 0 0 0 .937-.024l6.5-19a.496.496 0 0 0-.635-.635l-19 6.5a.5.5 0 0 0-.024.937l7.93 3.18a2 2 0 0 1 1.112 1.11z" /> <path d="m21.854 2.147-10.94 10.939" />',
  'pin': '<path d="M12 17v5" /> <path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z" />',
  'check': '<path d="M20 6 9 17l-5-5" />',
  'x': '<path d="M18 6 6 18" /> <path d="m6 6 12 12" />',
  'pause': '<rect x="14" y="4" width="4" height="16" rx="1" /> <rect x="6" y="4" width="4" height="16" rx="1" />',
  'rotate-cw': '<path d="M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8" /> <path d="M21 3v5h-5" />',
  'step-forward': '<line x1="6" x2="6" y1="4" y2="20" /> <polygon points="10,4 20,12 10,20" />',
  'arrow-down-to-line': '<path d="M12 17V3" /> <path d="m6 11 6 6 6-6" /> <path d="M19 21H5" />',
  'arrow-up-from-line': '<path d="m18 9-6-6-6 6" /> <path d="M12 3v14" /> <path d="M5 21h14" />',
  'circle-dot': '<circle cx="12" cy="12" r="10" /> <circle cx="12" cy="12" r="1" />',
  'git-pull-request': '<circle cx="18" cy="18" r="3" /> <circle cx="6" cy="6" r="3" /> <path d="M13 6h3a2 2 0 0 1 2 2v7" /> <line x1="6" x2="6" y1="9" y2="21" />',
  'circle-check': '<circle cx="12" cy="12" r="10" /> <path d="m9 12 2 2 4-4" />',
  'circle-x': '<circle cx="12" cy="12" r="10" /> <path d="m15 9-6 6" /> <path d="m9 9 6 6" />',
  'copy': '<rect width="14" height="14" x="8" y="8" rx="2" ry="2" /> <path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" />',
  'external-link': '<path d="M15 3h6v6" /> <path d="M10 14 21 3" /> <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />',
  'chevron-down': '<path d="m6 9 6 6 6-6" />',
  'file-code': '<path d="M10 12.5 8 15l2 2.5" /> <path d="m14 12.5 2 2.5-2 2.5" /> <path d="M14 2v4a2 2 0 0 0 2 2h4" /> <path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7z" />',
  'panel-top': '<rect width="18" height="18" x="3" y="3" rx="2" /> <path d="M3 9h18" />',
  'sun-moon': '<path d="M12 8a2.83 2.83 0 0 0 4 4 4 4 0 1 1-4-4" /> <path d="M12 2v2" /> <path d="M12 20v2" /> <path d="m4.9 4.9 1.4 1.4" /> <path d="m17.7 17.7 1.4 1.4" /> <path d="M2 12h2" /> <path d="M20 12h2" /> <path d="m6.3 17.7-1.4 1.4" /> <path d="m19.1 4.9-1.4 1.4" />',
  'shield-check': '<path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z" /> <path d="m9 12 2 2 4-4" />',
  'git-branch': '<line x1="6" x2="6" y1="3" y2="15" /> <circle cx="18" cy="6" r="3" /> <circle cx="6" cy="18" r="3" /> <path d="M18 9a9 9 0 0 1-9 9" />',
  'circle-alert': '<circle cx="12" cy="12" r="10" /> <line x1="12" x2="12" y1="8" y2="12" /> <line x1="12" x2="12.01" y1="16" y2="16" />',
} as const;

export type WorkbenchIcon = keyof typeof SHAPES;

export function workbenchIcon(name: WorkbenchIcon, className: string): SVGSVGElement {
  const icon = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  icon.setAttribute('viewBox', '0 0 24 24');
  icon.setAttribute('aria-hidden', 'true');
  icon.setAttribute('focusable', 'false');
  icon.setAttribute('class', className);
  icon.setAttribute('data-icon', name);
  // The markup is a closed, compile-time map, never workspace or user input.
  icon.innerHTML = SHAPES[name];
  return icon;
}
