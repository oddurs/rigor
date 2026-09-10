import * as stylex from '@stylexjs/stylex';

const DARK = '@media (prefers-color-scheme: dark)';

export const c = stylex.defineVars({
  bg: { default: '#ffffff', [DARK]: '#0b0c0e' },
  surface: { default: '#f6f7f9', [DARK]: '#13151a' },
  ink: { default: '#0f1114', [DARK]: '#f2f4f7' },
  inkSoft: { default: '#3c4149', [DARK]: '#c4cad3' },
  muted: { default: '#6b737d', [DARK]: '#8a9199' },
  hairline: { default: '#e3e6ea', [DARK]: '#23262c' },
  accent: { default: '#00a259', [DARK]: '#2fd68b' },
  accentSoft: { default: '#e8f8ef', [DARK]: '#0e2a1d' },
  onAccent: { default: '#ffffff', [DARK]: '#04150d' },

  // The terminal frame stays dark in both themes, because a terminal does.
  termBg: '#101317',
  termInk: '#cdd4de',
  termFaint: '#616a75',
  termRule: '#2a2f37',
  termDot: '#39404a',
  termGood: '#3ddc91',
  termBad: '#ff6b62',
  termBusy: '#f2b544',
});

export const f = stylex.defineVars({
  sans: 'ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif',
  mono: 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace',
});
