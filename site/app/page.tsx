import * as stylex from '@stylexjs/stylex';
import { Terminal } from './terminal';
import { c, f } from './tokens.stylex';

const REPO = 'https://github.com/oddurs/rigor';

const ANSWERS = [
  {
    title: 'What can I merge?',
    body: 'Ready holds only pull requests with green checks, no conflicts, and either an approval or no review requirement. If it appears there, the merge button works.',
  },
  {
    title: 'What is actually blocked?',
    body: 'Blocked means a real obstacle: red CI, requested changes, or a conflict with the base. A pull request waiting on its first review is not blocked, it is just in the queue.',
  },
  {
    title: 'Which worktrees can I delete?',
    body: 'Every worktree is mapped to its branch and pull request. One whose branch has landed and whose tree is clean is marked removable. One holding uncommitted or unpushed work never is.',
  },
];

const VIEWS = [
  ['Ready', 'Green, conflict-free, and approved or not requiring review.'],
  ['Mine', 'Open pull requests you authored.'],
  ['Review', 'Open pull requests waiting on your review.'],
  ['Blocked', 'Red CI, requested changes, or conflicts with the base.'],
  ['All', 'Every open pull request on the repository.'],
  ['Worktrees', 'Each worktree, its branch, its pull request, and its local state.'],
];

const KEYS = [
  ['1–6, Tab', 'switch view'],
  ['j / k, ↓ / ↑', 'move the selection'],
  ['Enter, o', 'open the pull request in the browser'],
  ['c', 'open its checks page'],
  ['y', 'copy the pull request URL'],
  ['/', 'filter by title, branch, author or label'],
  ['s', 'sort by recency or by merge-readiness'],
  ['r', 'refresh now'],
  ['?', 'every key, including the mouse bindings'],
];

export default function Page() {
  return (
    <>
      <header {...stylex.props(s.header)}>
        <div {...stylex.props(s.shell, s.headerRow)}>
          <a href="#top" {...stylex.props(s.wordmark)}>
            rigor
          </a>
          <nav {...stylex.props(s.nav)}>
            <a href="#views" {...stylex.props(s.navLink)}>
              Views
            </a>
            <a href="#docs" {...stylex.props(s.navLink)}>
              Docs
            </a>
            <a href={REPO} {...stylex.props(s.navLink)}>
              GitHub
            </a>
          </nav>
        </div>
      </header>

      <main id="top">
        <section {...stylex.props(s.shell, s.hero)}>
          <p {...stylex.props(s.eyebrow)}>Open source · MIT · Rust</p>
          <h1 {...stylex.props(s.h1)}>
            See what you can
            <br />
            merge.
          </h1>
          <p {...stylex.props(s.lede)}>
            rigor is a terminal dashboard for the pull requests you have out on a
            repository. It shows what is green, what is stuck, and which worktrees you can
            throw away — then hands you to the browser to merge.
          </p>
          <div {...stylex.props(s.ctaRow)}>
            <code {...stylex.props(s.install)}>
              cargo install --git https://github.com/oddurs/rigor
            </code>
            <a href={REPO} {...stylex.props(s.cta)}>
              View source →
            </a>
          </div>
        </section>

        <section {...stylex.props(s.termShell, s.termWrap)}>
          <Terminal />
        </section>

        <section {...stylex.props(s.shell, s.block)}>
          <div {...stylex.props(s.threeUp)}>
            {ANSWERS.map((a) => (
              <div key={a.title}>
                <h2 {...stylex.props(s.h3)}>{a.title}</h2>
                <p {...stylex.props(s.body)}>{a.body}</p>
              </div>
            ))}
          </div>
        </section>

        <section id="views" {...stylex.props(s.shell, s.block)}>
          <h2 {...stylex.props(s.h2)}>Six views, one keystroke apart</h2>
          <p {...stylex.props(s.lede, s.tight)}>
            Number keys, Tab, or a click. The tab bar is configurable, so a repository
            with different traffic can show a different set.
          </p>
          <ul {...stylex.props(s.viewGrid)}>
            {VIEWS.map(([name, desc]) => (
              <li key={name} {...stylex.props(s.card)}>
                <span {...stylex.props(s.cardTitle)}>{name}</span>
                <span {...stylex.props(s.cardBody)}>{desc}</span>
              </li>
            ))}
          </ul>
        </section>

        <section id="docs" {...stylex.props(s.shell, s.block)}>
          <h2 {...stylex.props(s.h2)}>Docs</h2>

          <h3 {...stylex.props(s.h4)}>Install</h3>
          <p {...stylex.props(s.body)}>
            rigor reads everything through the <Mono>gh</Mono> CLI, so it uses your
            existing authentication and never asks for a token. You need <Mono>gh</Mono>{' '}
            authenticated and a Rust toolchain.
          </p>
          <Code>{`cargo install --git https://github.com/oddurs/rigor`}</Code>

          <h3 {...stylex.props(s.h4)}>Quickstart</h3>
          <p {...stylex.props(s.body)}>
            Run it inside any checkout. It resolves the repository from the{' '}
            <Mono>origin</Mono> remote and opens on Ready.
          </p>
          <Code>{`cd ~/src/some-repo\nrigor`}</Code>

          <h3 {...stylex.props(s.h4)}>Keys</h3>
          <dl {...stylex.props(s.keys)}>
            {KEYS.map(([k, d]) => (
              <div key={k} {...stylex.props(s.keyRow)}>
                <dt {...stylex.props(s.kbd)}>{k}</dt>
                <dd {...stylex.props(s.keyDesc)}>{d}</dd>
              </div>
            ))}
          </dl>
          <p {...stylex.props(s.body)}>
            The mouse works too: click a tab or a row, double-click a row to open it,
            click a check run to open that job, scroll either pane.
          </p>

          <h3 {...stylex.props(s.h4)}>CI status</h3>
          <p {...stylex.props(s.body)}>
            Each row carries a rolled-up glyph with counts — <Mono>✓ 19</Mono>,{' '}
            <Mono>✗ 1/19</Mono>, <Mono>◐ 15/19</Mono>. Selecting a pull request expands
            every check run with its state and duration. Repeated runs of the same check
            are collapsed to the newest, and skipped jobs are counted separately from
            passing ones — a repository with path filters skips most of its matrix on most
            pull requests, and folding those into “passed” makes everything look better
            tested than it is.
          </p>

          <h3 {...stylex.props(s.h4)}>Configuration</h3>
          <p {...stylex.props(s.body)}>
            Everything is optional. Write a commented starter and edit it:
          </p>
          <Code>{`rigor --init-config   # ~/.config/rigor/config.toml`}</Code>
          <p {...stylex.props(s.body)}>
            A repository-local <Mono>.rigor.toml</Mono> overrides the user config.
          </p>
          <Code>{`default_view    = "ready"
views           = ["ready", "mine", "review", "blocked", "all", "worktrees"]
refresh_secs    = 90
layout          = "auto"    # auto | split | stack
worktree_status = true`}</Code>

          <h3 {...stylex.props(s.h4)}>Theming</h3>
          <p {...stylex.props(s.body)}>
            Colours resolve through your terminal’s own ANSI palette, so rigor renders in
            whatever theme its parent is running — nothing is pinned to the fixed 256
            colour cube. Override any slot with an ANSI name, a palette index, or a hex
            value.
          </p>
          <Code>{`[theme]
accent  = "cyan"
success = "#2fd68b"
failure = "red"`}</Code>
          <p {...stylex.props(s.body)}>
            <Mono>RIGOR_THEME</Mono> or <Mono>HERDR_THEME_FILE</Mono> point at a theme
            file, which lets a parent shell hand its palette down at launch.{' '}
            <Mono>NO_COLOR</Mono> is honoured.
          </p>
        </section>
      </main>

      <footer {...stylex.props(s.footer)}>
        <div {...stylex.props(s.shell, s.footerRow)}>
          <span {...stylex.props(s.footNote)}>MIT © 2026 Oddur Sigurdsson</span>
          <span {...stylex.props(s.footLinks)}>
            <a href={REPO} {...stylex.props(s.navLink)}>
              Source
            </a>
            <a href={`${REPO}/issues`} {...stylex.props(s.navLink)}>
              Issues
            </a>
          </span>
        </div>
      </footer>
    </>
  );
}

function Mono({ children }: { children: React.ReactNode }) {
  return <code {...stylex.props(s.inlineCode)}>{children}</code>;
}

function Code({ children }: { children: string }) {
  return (
    <pre {...stylex.props(s.code)}>
      <code>{children}</code>
    </pre>
  );
}

const s = stylex.create({
  shell: {
    width: '100%',
    maxWidth: '1080px',
    marginInline: 'auto',
    paddingInline: { default: '32px', '@media (max-width: 700px)': '20px' },
  },

  header: {
    position: 'sticky',
    top: 0,
    zIndex: 10,
    backgroundColor: c.bg,
    borderBottom: `1px solid ${c.hairline}`,
  },
  headerRow: {
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'space-between',
    height: '60px',
  },
  wordmark: {
    fontSize: '17px',
    fontWeight: 600,
    letterSpacing: '-0.02em',
    color: c.ink,
  },
  nav: { display: 'flex', gap: '26px' },
  navLink: {
    fontSize: '14px',
    color: { default: c.muted, ':hover': c.ink },
  },

  hero: { paddingTop: { default: '96px', '@media (max-width: 700px)': '56px' } },
  eyebrow: {
    fontSize: '12px',
    fontWeight: 600,
    letterSpacing: '0.09em',
    textTransform: 'uppercase',
    color: c.accent,
    marginBottom: '20px',
  },
  h1: {
    fontSize: {
      default: 'clamp(3rem, 7vw, 5.4rem)',
      '@media (max-width: 700px)': '2.7rem',
    },
    fontWeight: 700,
    lineHeight: 0.97,
    letterSpacing: '-0.04em',
    color: c.ink,
  },
  lede: {
    marginTop: '26px',
    maxWidth: '54ch',
    fontSize: { default: '19px', '@media (max-width: 700px)': '17px' },
    lineHeight: 1.55,
    color: c.inkSoft,
  },
  tight: { marginTop: '14px' },

  ctaRow: {
    display: 'flex',
    flexWrap: 'wrap',
    alignItems: 'center',
    gap: '16px',
    marginTop: '36px',
  },
  install: {
    paddingBlock: '13px',
    paddingInline: '18px',
    borderRadius: '9px',
    backgroundColor: c.surface,
    border: `1px solid ${c.hairline}`,
    fontFamily: f.mono,
    fontSize: { default: '13.5px', '@media (max-width: 700px)': '11.5px' },
    color: c.ink,
    overflowX: 'auto',
    whiteSpace: 'nowrap',
    maxWidth: '100%',
  },
  cta: {
    fontSize: '15px',
    fontWeight: 500,
    color: { default: c.ink, ':hover': c.accent },
  },

  termShell: {
    width: '100%',
    maxWidth: '1200px',
    marginInline: 'auto',
    paddingInline: { default: '32px', '@media (max-width: 700px)': '20px' },
  },
  termWrap: { marginTop: '56px' },

  block: { marginTop: { default: '104px', '@media (max-width: 700px)': '68px' } },
  threeUp: {
    display: 'grid',
    gridTemplateColumns: {
      default: 'repeat(3, minmax(0, 1fr))',
      '@media (max-width: 860px)': '1fr',
    },
    gap: '40px',
    paddingTop: '48px',
    borderTop: `1px solid ${c.hairline}`,
  },

  h2: {
    fontSize: {
      default: 'clamp(1.9rem, 3.4vw, 2.6rem)',
      '@media (max-width: 700px)': '1.8rem',
    },
    fontWeight: 700,
    letterSpacing: '-0.03em',
    lineHeight: 1.08,
    color: c.ink,
  },
  h3: {
    fontSize: '18px',
    fontWeight: 600,
    letterSpacing: '-0.015em',
    color: c.ink,
    marginBottom: '12px',
  },
  h4: {
    fontSize: '15px',
    fontWeight: 600,
    letterSpacing: '0.02em',
    textTransform: 'uppercase',
    color: c.muted,
    marginTop: '52px',
    marginBottom: '14px',
  },
  body: {
    maxWidth: '68ch',
    marginTop: '12px',
    fontSize: '16px',
    lineHeight: 1.65,
    color: c.inkSoft,
  },

  viewGrid: {
    display: 'grid',
    gridTemplateColumns: {
      default: 'repeat(3, minmax(0, 1fr))',
      '@media (max-width: 860px)': 'repeat(2, minmax(0, 1fr))',
      '@media (max-width: 560px)': '1fr',
    },
    gap: '14px',
    marginTop: '36px',
  },
  card: {
    display: 'flex',
    flexDirection: 'column',
    gap: '8px',
    padding: '22px',
    borderRadius: '12px',
    backgroundColor: c.surface,
    border: `1px solid ${c.hairline}`,
  },
  cardTitle: {
    fontFamily: f.mono,
    fontSize: '13px',
    fontWeight: 600,
    color: c.accent,
  },
  cardBody: { fontSize: '15px', lineHeight: 1.5, color: c.inkSoft },

  code: {
    marginTop: '16px',
    padding: '18px',
    borderRadius: '10px',
    backgroundColor: c.surface,
    border: `1px solid ${c.hairline}`,
    overflowX: 'auto',
    fontFamily: f.mono,
    fontSize: '13px',
    lineHeight: 1.7,
    color: c.ink,
  },
  inlineCode: {
    paddingBlock: '2px',
    paddingInline: '5px',
    borderRadius: '4px',
    backgroundColor: c.surface,
    border: `1px solid ${c.hairline}`,
    fontFamily: f.mono,
    fontSize: '0.88em',
    color: c.ink,
  },

  keys: { marginTop: '18px', borderTop: `1px solid ${c.hairline}` },
  keyRow: {
    display: 'grid',
    gridTemplateColumns: { default: '190px 1fr', '@media (max-width: 560px)': '1fr' },
    gap: '10px',
    paddingBlock: '11px',
    borderBottom: `1px solid ${c.hairline}`,
  },
  kbd: { fontFamily: f.mono, fontSize: '13px', color: c.ink },
  keyDesc: { margin: 0, fontSize: '15px', color: c.inkSoft },

  footer: {
    marginTop: '112px',
    paddingBlock: '32px',
    borderTop: `1px solid ${c.hairline}`,
  },
  footerRow: {
    display: 'flex',
    flexWrap: 'wrap',
    gap: '16px',
    alignItems: 'center',
    justifyContent: 'space-between',
  },
  footNote: { fontSize: '14px', color: c.muted },
  footLinks: { display: 'flex', gap: '22px' },
});
