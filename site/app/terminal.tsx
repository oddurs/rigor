import * as stylex from '@stylexjs/stylex';
import { c, f } from './tokens.stylex';

// A real frame, captured from the renderer's own test fixtures — the same code
// path that draws the dashboard, run against placeholder data.
const FRAME = [
  'rigor  acme/widget  ⎇ chore/release-notes  @octocat',
  ' 1 Ready 1  2 Mine 2  3 Review 1  4 Blocked 1  5 All 3  6 Worktrees 5                                                                 1 removable   sort recent',
  '▌#4846  ✗ 1/3   ◌ ⌂ Give the read path a retry budget                     octocat       3m   │ #4846 Give the read path a retry budget',
  ' #4840  ✓ 3     ✔ ⌂ Move the palette onto color tokens                   octocat       2h   │ octocat · 3m ago · +220 −24 · 6 files · 2 comments · → main',
  ' #4801  ◐ 2/3   ⚠   draft · Treat a filtered empty result as unknown      agent-bot     3d   │  review    review required',
  '                                                                                             │  merge     no conflicts',
  '                                                                                             │  labels    parser, backend',
  '                                                                                             │  branch    retry-budget',
  '                                                                                             │  worktree  widget-wt/retry-budget  ● 3 uncommitted  commit 3m ag…',
  '                                                                                             │  CHECKS   1 passed · 1 failing · 1 running',
  '                                                                                             │  ✗ e2e (web)                                         4m03s',
  '                                                                                             │  ◐ typecheck                                         1m12s',
  '                                                                                             │  ✓ build                                             1m42s',
  ' o open   c checks   y copy   / filter   s sort   r refresh   ? help   q quit',
];

const TOKEN = /#\d{3,5}|[✓✔]|[✗⚠]|[◐●]|▌|[⌂◌]|│/g;

type Kind = 'good' | 'bad' | 'busy' | 'bar' | 'rule' | 'faint' | 'num' | 'text';

function kindOf(tok: string): Kind {
  switch (tok) {
    case '✓':
    case '✔':
      return 'good';
    case '✗':
    case '⚠':
      return 'bad';
    case '◐':
    case '●':
      return 'busy';
    case '▌':
      return 'bar';
    case '│':
      return 'rule';
    case '⌂':
    case '◌':
      return 'faint';
    default:
      return 'num';
  }
}

/** Split a line into styled runs. A fresh regex per call: a global one carries
 *  `lastIndex` between calls and would silently split every other line wrongly. */
function segments(line: string): { text: string; kind: Kind }[] {
  const out: { text: string; kind: Kind }[] = [];
  const re = new RegExp(TOKEN.source, 'g');
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(line)) !== null) {
    if (m.index > last) out.push({ text: line.slice(last, m.index), kind: 'text' });
    out.push({ text: m[0], kind: kindOf(m[0]) });
    last = m.index + m[0].length;
  }
  if (last < line.length) out.push({ text: line.slice(last), kind: 'text' });
  return out;
}

export function Terminal() {
  return (
    <figure {...stylex.props(s.wrap)}>
      <div {...stylex.props(s.chrome)}>
        <span {...stylex.props(s.dot)} />
        <span {...stylex.props(s.dot)} />
        <span {...stylex.props(s.dot)} />
        <span {...stylex.props(s.chromeLabel)}>rigor — acme/widget</span>
      </div>
      <pre {...stylex.props(s.pre)} aria-label="rigor running in a terminal">
        {FRAME.map((line, i) => (
          <span key={i} {...stylex.props(s.line, i === 0 && s.header)}>
            {segments(line).map((seg, j) => (
              <span key={j} {...stylex.props(s[seg.kind])}>
                {seg.text}
              </span>
            ))}
            {'\n'}
          </span>
        ))}
      </pre>
    </figure>
  );
}

const s = stylex.create({
  wrap: {
    margin: 0,
    maxWidth: '100%',
    borderRadius: '12px',
    overflow: 'hidden',
    backgroundColor: c.termBg,
    boxShadow: '0 24px 60px -24px rgba(15, 17, 20, 0.45)',
    border: `1px solid ${c.termRule}`,
  },
  chrome: {
    display: 'flex',
    alignItems: 'center',
    gap: '7px',
    paddingBlock: '11px',
    paddingInline: '14px',
    borderBottom: `1px solid ${c.termRule}`,
  },
  dot: {
    width: '9px',
    height: '9px',
    borderRadius: '50%',
    backgroundColor: c.termDot,
  },
  chromeLabel: {
    marginInlineStart: '8px',
    fontFamily: f.mono,
    fontSize: '11px',
    letterSpacing: '0.02em',
    color: c.termFaint,
  },
  pre: {
    overflowX: 'auto',
    paddingBlock: '16px',
    paddingInline: '18px',
    fontFamily: f.mono,
    // 160 columns at ~0.6em advance: 10.5px keeps the whole frame inside the
    // card on a desktop viewport instead of silently clipping the detail pane.
    fontSize: {
      default: '10.5px',
      '@media (max-width: 1180px)': '9.5px',
      '@media (max-width: 760px)': '8px',
    },
    lineHeight: 1.55,
    color: c.termFaint,
    tabSize: 4,
  },
  line: { display: 'block', whiteSpace: 'pre' },
  header: { color: c.termInk },
  good: { color: c.termGood },
  bad: { color: c.termBad },
  busy: { color: c.termBusy },
  bar: { color: c.termGood },
  rule: { color: c.termRule },
  faint: { color: c.termFaint },
  num: { color: c.termInk },
  text: { color: c.termFaint },
});
