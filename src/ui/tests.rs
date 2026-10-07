//! Layout checks. These render real frames into an in-memory backend, so
//! column alignment and width fallbacks are verified without a terminal.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::draw;
use crate::app::{App, Mode, Sort, WtState};
use crate::config::{LayoutMode, Settings, View};
use crate::model::{
    Check, CheckState, Mergeable, MergedPr, Pr, RepoInfo, ReviewDecision, Worktree, WorktreeStatus,
};
use crate::theme::Theme;
use crate::util::now_secs;

/// The instant every fixture is built and rendered at. Arbitrary but fixed.
const NOW: i64 = 1_800_000_000;

fn sample_app() -> App {
    sample_app_and_replies().0
}

/// The app with the channel its background work answers on, for the tests
/// that follow a reply through.
fn sample_app_and_replies() -> (App, std::sync::mpsc::Receiver<crate::app::Msg>) {
    crate::util::freeze_time(NOW);
    let repo = RepoInfo {
        owner: "acme".into(),
        name: "widget".into(),
        default_branch: "main".into(),
        root: "/home/dev/src/widget".into(),
        current_branch: Some("chore/release-notes".into()),
    };
    // worktree_status off: no git shell-outs against fake paths
    let settings = Settings {
        worktree_status: false,
        ..Settings::default()
    };
    let (mut a, rx) = App::new(repo, settings, Theme::default());
    a.home = Some("/home/dev".into());
    let now = now_secs();

    let prs = sample_prs(now);

    // Route through the real message path so the merged-branch index is built
    // the way a live fetch builds it.
    a.on_msg(crate::app::Msg::Prs(Ok(crate::github::Fetched {
        budget: None,
        viewer: "octocat".into(),
        default_branch: "main".into(),
        prs,
        merged: vec![
            MergedPr {
                number: 4700,
                title: "Drop the monospace count chips".into(),
                url: "https://github.com/acme/widget/pull/4700".into(),
                head_ref: "drop-mono".into(),
                head_oid: "0f1e2d3c4b5a6978".into(),
                merged_at: now - 86400 * 4,
            },
            MergedPr {
                number: 4701,
                title: "Fix the row hover state".into(),
                url: "https://github.com/acme/widget/pull/4701".into(),
                head_ref: "row-hover".into(),
                head_oid: "0f1e2d3c4b5a6978".into(),
                merged_at: now - 86400 * 5,
            },
        ],
    })));

    a.on_msg(crate::app::Msg::Worktrees(Ok(sample_worktrees(now))));
    (a, rx)
}

/// Three PRs covering the states that matter: failing and dirty, approved and
/// green, and a draft that conflicts and waits on review.
fn sample_prs(now: i64) -> Vec<Pr> {
    let prs = vec![
        pr(
            4846,
            "Give the read path a retry budget",
            "octocat",
            "retry-budget",
            CheckState::Failure,
            Some(ReviewDecision::ReviewRequired),
            now - 200,
        ),
        pr(
            4840,
            "Move the palette onto color tokens",
            "octocat",
            "color-tokens",
            CheckState::Success,
            Some(ReviewDecision::Approved),
            now - 7200,
        ),
        pr(
            4801,
            "Treat a filtered empty result as unknown",
            "agent-bot",
            "issue-412",
            CheckState::Pending,
            None,
            now - 86400 * 3,
        ),
    ];
    let mut prs = prs;
    prs[1].assignees = vec!["octocat".into()];
    prs[2].review_requests = vec!["octocat".into()];
    prs[2].is_draft = true;
    prs[2].mergeable = Mergeable::Conflicting;
    prs
}

/// The main worktree, a dirty desk, a clean one with an open PR, one whose
/// branch landed (removable), and one that landed but still holds work.
fn sample_worktrees(now: i64) -> Vec<Worktree> {
    vec![
        wt(
            "/home/dev/src/widget",
            "chore/release-notes",
            true,
            0,
            now - 900,
        ),
        wt(
            "/home/dev/src/widget-wt/retry-budget",
            "retry-budget",
            false,
            3,
            now - 200,
        ),
        wt(
            "/home/dev/src/widget-wt/color-tokens",
            "color-tokens",
            false,
            0,
            now - 7200,
        ),
        // branch landed and the desk is clean -> collectable
        wt(
            "/home/dev/src/widget-wt/drop-mono",
            "drop-mono",
            false,
            0,
            now - 86400 * 4,
        ),
        // branch landed but the desk still holds uncommitted work -> keep it
        wt(
            "/home/dev/src/widget-wt/row-hover",
            "row-hover",
            false,
            4,
            now - 86400 * 5,
        ),
    ]
}

fn pr(
    number: u64,
    title: &str,
    author: &str,
    head: &str,
    rollup: CheckState,
    review: Option<ReviewDecision>,
    updated: i64,
) -> Pr {
    let now = now_secs();
    Pr {
        number,
        title: title.into(),
        url: format!("https://github.com/acme/widget/pull/{number}"),
        is_draft: false,
        updated_at: updated,
        author: author.into(),
        head_ref: head.into(),
        head_oid: crate::drafts::tests::HEAD.into(),
        base_ref: "main".into(),
        additions: 220,
        deletions: 24,
        changed_files: 6,
        mergeable: Mergeable::Clean,
        review_decision: review,
        assignees: vec![],
        review_requests: vec![],
        labels: vec!["parser".into(), "backend".into()],
        comments: 2,
        checks: vec![
            Check {
                name: "e2e (web)".into(),
                state: CheckState::Failure,
                url: Some("https://x/1".into()),
                started_at: Some(now - 300),
                completed_at: Some(now - 57),
            },
            Check {
                name: "typecheck".into(),
                state: CheckState::Pending,
                url: Some("https://x/2".into()),
                started_at: Some(now - 72),
                completed_at: None,
            },
            Check {
                name: "build".into(),
                state: CheckState::Success,
                url: Some("https://x/3".into()),
                started_at: Some(now - 402),
                completed_at: Some(now - 300),
            },
        ],
        rollup,
    }
}

fn wt(path: &str, branch: &str, is_main: bool, dirty: usize, last: i64) -> Worktree {
    Worktree {
        path: path.into(),
        branch: Some(branch.into()),
        head: "0f1e2d3c4b5a6978".into(),
        is_main,
        detached: false,
        status: Some(WorktreeStatus {
            dirty,
            unpushed: 0,
            published: true,
            last_commit_at: Some(last),
            last_subject: "Give the read path a retry budget".into(),
            last_author: "octocat".into(),
        }),
    }
}

fn render(a: &mut App, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| draw(f, a)).unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Nothing may overflow the frame at any of the widths we adapt to.
#[test]
fn renders_at_every_width() {
    let mut a = sample_app();
    for view in View::ALL {
        a.set_view(view);
        for help in [false, true] {
            a.mode = if help { Mode::Help } else { Mode::Browse };
            for (w, h) in [
                (200u16, 40u16),
                (140, 30),
                (110, 24),
                (80, 20),
                (52, 12),
                (40, 8),
            ] {
                let out = render(&mut a, w, h);
                assert!(
                    out.lines().all(|l| crate::util::width(l) <= w as usize),
                    "view {view:?} (help={help}) overflowed at {w}x{h}:\n{out}"
                );
            }
        }
    }
    a.mode = Mode::Browse;
}

/// The attention sort is what makes the dashboard actionable: approved-and-green
/// first, then whatever is blocking.
#[test]
fn attention_sort_puts_mergeable_first() {
    let mut a = sample_app();
    a.set_view(View::All);
    a.sort = Sort::Attention;
    a.rebuild();
    let order: Vec<u64> = a
        .rows
        .iter()
        .map(|r| match r {
            crate::app::Row::Pr(i) => a.prs[*i].number,
            crate::app::Row::Wt(_) | crate::app::Row::Draft(_) => {
                unreachable!("only pull requests in this view")
            }
        })
        .collect();
    // approved + green, then the failing one, then the conflicting draft
    assert_eq!(order, vec![4840, 4846, 4801]);
}

/// Clicking a row must land on the row that was drawn there.
#[test]
fn row_hitboxes_match_the_drawn_rows() {
    let mut a = sample_app();
    a.set_view(View::All);
    let _ = render(&mut a, 160, 30);
    assert_eq!(a.hits.rows.len(), a.rows.len());
    let (y0, i0) = a.hits.rows[0];
    let (y1, i1) = a.hits.rows[1];
    assert_eq!(y1 - y0, 1);
    assert_eq!((i0, i1), (0, 1));
    assert_eq!(a.hits.tabs.len(), a.settings.views.len());
}

/// A wrapped detail line would silently shift every row below it, so the
/// recorded check hitboxes must land on the rows the checks were drawn on.
#[test]
fn check_hitboxes_land_on_their_own_rows() {
    let mut a = sample_app();
    a.set_view(View::All);
    let mut term = Terminal::new(TestBackend::new(150, 30)).unwrap();
    term.draw(|f| draw(f, &mut a)).unwrap();
    let buf = term.backend().buffer().clone();

    let pr = a.selected_pr().unwrap().clone();
    assert!(!a.hits.checks.is_empty());
    for (y, idx) in &a.hits.checks {
        let row: String = (0..buf.area.width).map(|x| buf[(x, *y)].symbol()).collect();
        let name = &pr.checks[*idx].name;
        assert!(
            row.contains(name.split_whitespace().next().unwrap()),
            "hitbox for check {idx} ({name}) points at row {y}: {row:?}"
        );
    }
}

/// Both split orientations must produce a usable detail pane.
#[test]
fn both_layouts_render_the_detail_pane() {
    for (mode, w) in [(LayoutMode::Split, 160u16), (LayoutMode::Stack, 100)] {
        let mut a = sample_app();
        a.settings.layout = mode;
        a.set_view(View::All);
        let out = render(&mut a, w, 30);
        assert!(
            out.contains("checks    1 passed"),
            "{mode:?} lost the checks section:\n{out}"
        );
        assert!(
            out.contains("worktree"),
            "{mode:?} lost the worktree line:\n{out}"
        );
        assert!(
            out.contains("widget-wt/retry-budget") && out.contains("3 uncommitted"),
            "{mode:?} did not link the PR to its worktree:\n{out}"
        );
    }
}

/// Ready and Blocked are the two views the dashboard exists for; they must
/// partition on the actual merge blockers, and never claim a draft is ready.
#[test]
fn ready_and_blocked_select_the_right_prs() {
    let mut a = sample_app();

    a.set_view(View::Ready);
    let ready: Vec<u64> = a.rows.iter().map(|r| pr_num(&a, r)).collect();
    assert_eq!(
        ready,
        vec![4840],
        "only the approved, green, conflict-free PR"
    );

    a.set_view(View::Blocked);
    let mut blocked: Vec<u64> = a.rows.iter().map(|r| pr_num(&a, r)).collect();
    blocked.sort_unstable();
    assert_eq!(
        blocked,
        vec![4846],
        "the failing one; the draft is not 'blocked'"
    );
}

/// A desk is only collectable when its branch landed AND nothing local is at
/// risk. Getting this wrong would invite `git worktree remove` over real work.
#[test]
fn removable_worktrees_need_a_clean_desk() {
    let a = sample_app();
    let by_name = |n: &str| {
        a.worktrees
            .iter()
            .find(|w| w.name() == n)
            .map(|w| a.wt_state(w))
            .unwrap()
    };
    assert_eq!(by_name("drop-mono"), WtState::Removable);
    assert_eq!(
        by_name("row-hover"),
        WtState::Working,
        "4 uncommitted files"
    );
    assert_eq!(by_name("retry-budget"), WtState::Working);
    assert_eq!(
        by_name("color-tokens"),
        WtState::Idle,
        "open PR, nothing landed"
    );
    assert_eq!(
        by_name("widget"),
        WtState::Idle,
        "the main worktree is never collectable"
    );
    assert_eq!(a.removable_count(), 1);
}

fn pr_num(a: &App, r: &crate::app::Row) -> u64 {
    match r {
        crate::app::Row::Pr(i) => a.prs[*i].number,
        crate::app::Row::Wt(_) | crate::app::Row::Draft(_) => {
            unreachable!("only pull requests in this view")
        }
    }
}

fn frame(a: &mut App, w: u16, h: u16) -> ratatui::buffer::Buffer {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| draw(f, a)).unwrap();
    term.backend().buffer().clone()
}

fn row_text(buf: &ratatui::buffer::Buffer, y: u16) -> String {
    (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
}

/// The subnav used to read `1 Ready 2  2 Mine 14` — two bare digits side by
/// side, so a count could be mistaken for the next key. Tabs now lead with
/// their title and carry only a count.
#[test]
fn tabs_lead_with_their_title_not_a_key_digit() {
    let mut a = sample_app();
    let buf = frame(&mut a, 160, 24);
    let subnav = row_text(&buf, 1);
    assert!(subnav.starts_with(" Ready 1"), "subnav: {subnav:?}");
    assert!(
        !subnav.contains("1 Ready"),
        "key digit crept back: {subnav:?}"
    );
}

/// The active view is marked by a heavy underline on the rail that hugs its
/// label — not the padding around it. It is what makes the row read as a
/// subnav, and it survives `NO_COLOR` because it is a glyph, not a color.
#[test]
fn the_rail_underlines_exactly_the_active_tab() {
    for view in [View::Ready, View::Blocked, View::Worktrees] {
        let mut a = sample_app();
        a.set_view(view);
        let buf = frame(&mut a, 160, 24);
        let (rect, _) = *a.hits.tabs.iter().find(|(_, v)| *v == view).unwrap();
        let (lo, hi) = (rect.x + 1, rect.x + rect.width - 1);
        for x in 0..buf.area.width {
            let under = x >= lo && x < hi;
            let glyph = buf[(x, 2)].symbol();
            if under {
                assert_eq!(
                    glyph, "━",
                    "{view:?}: column {x} under the tab is not underlined"
                );
            } else {
                assert_ne!(
                    glyph, "━",
                    "{view:?}: column {x} outside the tab is underlined"
                );
            }
        }
    }
}

/// Side by side, the rail carries a `┬` in the same column as the detail
/// pane's divider, so the two rules join instead of crossing.
#[test]
fn the_rail_joins_the_detail_divider() {
    let mut a = sample_app();
    a.settings.layout = LayoutMode::Split;
    a.set_view(View::Ready); // keep the underline away from the junction
    let buf = frame(&mut a, 160, 24);
    let x = (0..160)
        .find(|&x| buf[(x, 2)].symbol() == "┬")
        .expect("no junction on the rail");
    assert_eq!(
        buf[(x, 3)].symbol(),
        "│",
        "the junction is not above the divider"
    );
}

/// Two counts mean something at a glance: work that is ready, and work that is
/// stuck. They take the success and failure colors when non-zero.
#[test]
fn ready_and_blocked_counts_carry_meaning_in_color() {
    let mut a = sample_app();
    a.set_view(View::All);
    let buf = frame(&mut a, 160, 24);
    let subnav = row_text(&buf, 1);
    let digit_after = |title: &str| {
        let col = subnav.find(title).unwrap() + title.len() + 1;
        buf[(u16::try_from(col).unwrap(), 1)].fg
    };
    assert_eq!(digit_after("Ready"), a.theme.success);
    assert_eq!(digit_after("Blocked"), a.theme.failure);
    assert_eq!(
        digit_after("Mine"),
        a.theme.muted,
        "an ordinary count stays quiet"
    );
}

/// The top nav gives up context in order of how little it helps — branch, then
/// user, then status — and always keeps the badge and the repository.
#[test]
fn the_nav_sheds_context_before_it_loses_the_repo() {
    let mut a = sample_app();
    let wide = row_text(&frame(&mut a, 160, 24), 0);
    assert!(wide.contains("⎇ chore/release-notes") && wide.contains("@octocat"));

    let narrow = row_text(&frame(&mut a, 44, 24), 0);
    assert!(
        narrow.contains("rigor") && narrow.contains("acme/widget"),
        "{narrow:?}"
    );
    assert!(
        !narrow.contains("⎇"),
        "the branch should go first: {narrow:?}"
    );
}

/// With a background the terminal reported, the selected row is a band that
/// runs the full width of the list — edge to edge, not just under the text.
#[test]
fn the_selected_row_is_a_full_width_band_when_the_terminal_reports_its_colors() {
    let mut a = sample_app();
    a.theme = Theme::resolve(
        &crate::theme::ThemeConfig::default(),
        None,
        crate::probe::Probed {
            fg: Some((0xd4, 0xd8, 0xde)),
            bg: Some((0x16, 0x18, 0x1c)),
        },
    )
    .unwrap();
    a.settings.layout = LayoutMode::Split;
    a.set_view(View::All);
    let buf = frame(&mut a, 160, 24);

    let (y, _) = a.hits.rows[0];
    let list = a.hits.list;
    for x in list.x..list.x + list.width {
        assert_eq!(
            buf[(x, y)].bg,
            a.theme.sel_bg,
            "column {x} of the selected row is unbanded"
        );
    }
    let (y2, _) = a.hits.rows[1];
    assert_ne!(
        buf[(list.x + 10, y2)].bg,
        a.theme.sel_bg,
        "an unselected row is banded"
    );
    assert_eq!(
        buf[(list.x, y)].symbol(),
        "▎",
        "the accent bar marks the edge"
    );
}

/// The subnav no longer paints a band behind the active tab — the underline
/// is the marker — so a derived band never leaks into the chrome.
#[test]
fn the_band_stays_out_of_the_tab_bar() {
    let mut a = sample_app();
    a.theme.sel_bg = ratatui::style::Color::Rgb(0x28, 0x2b, 0x30);
    a.set_view(View::Mine);
    let buf = frame(&mut a, 160, 24);
    for x in 0..buf.area.width {
        assert_ne!(
            buf[(x, 1)].bg,
            a.theme.sel_bg,
            "column {x} of the subnav is banded"
        );
    }
}

/// The status dot tells the user whether to trust the screen: green when
/// fresh, amber when stale, red when the last sync failed.
#[test]
fn the_sync_dot_reports_freshness() {
    let dot_color = |a: &mut App| {
        let buf = frame(a, 160, 24);
        let row = row_text(&buf, 0);
        let col =
            u16::try_from(row.chars().position(|c| c == '●').expect("no status dot")).unwrap();
        buf[(col, 0)].fg
    };

    let mut a = sample_app();
    a.schedule.last_refresh = now_secs() - 5;
    assert_eq!(dot_color(&mut a), a.theme.success);

    a.schedule.last_refresh = now_secs() - crate::util::secs(a.settings.refresh_secs) * 3;
    assert_eq!(dot_color(&mut a), a.theme.pending);

    a.error = Some("gh api graphql failed".into());
    assert_eq!(dot_color(&mut a), a.theme.failure);
}

/// An accepted filter is shown beside the list it narrows, not in the footer.
#[test]
fn an_active_filter_shows_in_the_subnav() {
    let mut a = sample_app();
    a.filter = "retry".into();
    a.rebuild();
    let buf = frame(&mut a, 160, 24);
    assert!(
        row_text(&buf, 1).contains("/retry"),
        "{:?}",
        row_text(&buf, 1)
    );
    assert!(
        !row_text(&buf, 23).contains("retry"),
        "the footer still shows it"
    );
}

fn footer(buf: &ratatui::buffer::Buffer) -> String {
    row_text(buf, buf.area.height - 1)
}

/// Only states that change what you do next earn a glyph in the list. "Review
/// required" is the resting state of nearly every PR, so it stays blank there.
#[test]
fn the_list_marks_only_notable_review_states() {
    let mut a = sample_app();
    a.set_view(View::All);
    let buf = frame(&mut a, 160, 24);
    let row_of = |n: &str| {
        (0..24)
            .map(|y| row_text(&buf, y))
            .find(|r| r.contains(n))
            .unwrap()
    };
    assert!(
        !row_of("#4846").contains('◌'),
        "review-required should be blank"
    );
    assert!(row_of("#4840").contains('✔'), "approved keeps its glyph");
}

/// Skipped jobs are counted, not listed: one line in place of a row each.
#[test]
fn skipped_checks_collapse_to_a_single_line() {
    let mut a = sample_app();
    let now = now_secs();
    for name in ["docs render", "design a11y", "worker corpora"] {
        a.prs[0].checks.push(Check {
            name: name.into(),
            state: CheckState::Skipped,
            url: None,
            started_at: Some(now - 10),
            completed_at: Some(now - 10),
        });
    }
    a.set_view(View::All);
    let buf = frame(&mut a, 160, 30);
    let text: Vec<String> = (0..30).map(|y| row_text(&buf, y)).collect();
    assert!(
        text.iter().any(|r| r.contains("– 3 skipped")),
        "no collapsed skip line"
    );
    assert!(
        !text.iter().any(|r| r.contains("design a11y")),
        "a skipped check is listed"
    );
}

/// An empty view points somewhere useful, and the detail pane stays out of it.
#[test]
fn an_empty_view_points_somewhere_useful() {
    let mut a = sample_app();
    a.settings.views = vec![View::Ready, View::Mine, View::Assigned, View::All];
    a.prs[1].assignees.clear();
    a.set_view(View::Assigned);
    let buf = frame(&mut a, 160, 24);
    let body: Vec<String> = (3..23).map(|y| row_text(&buf, y)).collect();
    assert!(
        body.iter().any(|r| r.contains("1 in Ready — press 1")),
        "{body:#?}"
    );
    assert!(
        !body.iter().any(|r| r.contains("Nothing selected")),
        "detail repeats the list"
    );
    let f = footer(&buf);
    assert!(
        !f.contains("checks") && !f.contains("copy"),
        "offers actions on nothing: {f:?}"
    );
}

/// Help and quit are how you find everything else, so they never clip.
#[test]
fn the_footer_keeps_help_and_quit_at_any_width() {
    let mut a = sample_app();
    for w in [160u16, 90, 60] {
        let f = footer(&frame(&mut a, w, 24));
        assert!(
            f.contains("? help") && f.contains("q quit"),
            "at {w}: {f:?}"
        );
    }
}

/// Typing a filter shows how far the list has narrowed.
#[test]
fn the_filter_prompt_counts_matches() {
    let mut a = sample_app();
    a.set_view(View::All);
    a.mode = Mode::Filter;
    a.filter = "retry".into();
    a.rebuild();
    let f = footer(&frame(&mut a, 160, 24));
    assert!(f.contains("/retry") && f.contains("1 of 3"), "{f:?}");
}

/// The help modal sits on a dimmed background, so it reads as a layer.
#[test]
fn the_help_modal_dims_what_is_behind_it() {
    let mut a = sample_app();
    a.mode = Mode::Help;
    let buf = frame(&mut a, 160, 40);
    assert!(buf[(0, 0)].modifier.contains(ratatui::style::Modifier::DIM));
}

/// A list longer than its pane says where you are in it, on the rail.
#[test]
fn a_long_list_shows_its_position_on_the_rail() {
    // The body always keeps at least three rows, so the three fixture PRs can
    // never overflow it; add enough to spill past a short pane.
    let mut a = sample_app();
    let extra: Vec<Pr> = (0..5)
        .map(|i| {
            let mut p = a.prs[1].clone();
            p.number = 5000 + i;
            p
        })
        .collect();
    a.prs.extend(extra);
    a.settings.layout = LayoutMode::Split;
    a.set_view(View::All);
    a.move_sel(1);
    let buf = frame(&mut a, 160, 10);
    let rail = row_text(&buf, 2);
    assert!(rail.contains("2/8"), "{rail:?}");
    let marker_end = rail.find("2/8").unwrap() + 3;
    let junction = rail.find('┬').unwrap();
    assert!(
        marker_end < junction,
        "the marker sits inside the list's stretch of rail"
    );

    let tall = frame(&mut a, 160, 40);
    assert!(
        !row_text(&tall, 2).contains("/8"),
        "no marker when everything fits"
    );
}

/// Worktree names keep a gutter however long they are, and only the selected
/// one is bold — a column of bold names has no hierarchy left in it.
#[test]
fn worktree_names_keep_a_gutter_and_only_the_selection_is_bold() {
    let mut a = sample_app();
    let now = now_secs();
    let mut wts = a.worktrees.clone();
    wts.push(wt(
        "/home/dev/src/widget-wt/onboarding-mobile-compression-pass",
        "onboarding-mobile-compression-pass",
        false,
        0,
        now - 60,
    ));
    a.on_msg(crate::app::Msg::Worktrees(Ok(wts)));
    a.set_view(View::Worktrees);
    let buf = frame(&mut a, 160, 24);

    let (long_y, _) = *a
        .hits
        .rows
        .iter()
        .find(|(y, _)| row_text(&buf, *y).contains("onboarding"))
        .unwrap();
    // glyph column (3) + name column (24): the last column of the name is blank
    assert_eq!(
        buf[(26, long_y)].symbol(),
        " ",
        "{:?}",
        row_text(&buf, long_y)
    );

    let bold = |y: u16| {
        buf[(4, y)]
            .modifier
            .contains(ratatui::style::Modifier::BOLD)
    };
    let (sel_y, _) = a.hits.rows[a.selected];
    assert!(bold(sel_y), "the selected name should be bold");
    let other = a.hits.rows.iter().find(|(y, _)| *y != sel_y).unwrap().0;
    assert!(!bold(other), "an unselected name is bold");
}

proptest::proptest! {
    /// SECURITY.md names this as rigor's attack surface: strings from a
    /// repository or the GitHub API are drawn into a terminal. A title, branch,
    /// author, label or check name carrying escape sequences must never reach
    /// the screen as live control characters.
    #[test]
    fn untrusted_strings_never_reach_the_terminal_as_control_characters(
        payload in proptest::string::string_regex(
            "[a-z ]{0,6}(\\x1b\\[31m|\\x1b\\]8;;https://evil\\x07|\\x07|\\x08|\\r|\\x9b2J|\\u{202e}|\\u{2067}|\\x00)[a-z ]{0,6}"
        ).unwrap()
    ) {
        let mut a = sample_app();
        a.prs[0].title = format!("t{payload}");
        a.prs[0].head_ref = format!("b{payload}");
        a.prs[0].author = format!("u{payload}");
        a.prs[0].labels = vec![format!("l{payload}")];
        a.prs[0].checks[0].name = format!("c{payload}");
        a.set_view(View::All);
        let buf = frame(&mut a, 160, 24);
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let sym = buf[(x, y)].symbol();
                // Control characters could drive the terminal; bidi overrides
                // and isolates could make a title display as something else.
                let dangerous = |c: char| {
                    c.is_control()
                        || ('\u{202a}'..='\u{202e}').contains(&c)
                        || ('\u{2066}'..='\u{2069}').contains(&c)
                };
                proptest::prop_assert!(
                    !sym.chars().any(dangerous),
                    "dangerous character {:?} at ({x},{y}) from payload {:?}", sym, payload
                );
            }
        }
    }
}

// ------------------------------------------------------------------ keys

fn press(a: &mut App, keys: &str) {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for ch in keys.chars() {
        let code = match ch {
            '\n' => KeyCode::Enter,
            '\x1b' => KeyCode::Esc,
            '\x08' => KeyCode::Backspace,
            '\t' => KeyCode::Tab,
            c => KeyCode::Char(c),
        };
        crate::event::key(a, KeyEvent::new(code, KeyModifiers::NONE));
    }
}

/// The keyboard is the primary interface; its contract is pinned here.
#[test]
fn keys_move_the_selection_and_switch_views_by_position() {
    let mut a = sample_app();
    a.set_view(View::All);
    press(&mut a, "j");
    assert_eq!(a.selected, 1);
    press(&mut a, "G");
    assert_eq!(a.selected, a.rows.len() - 1);
    press(&mut a, "k");
    assert_eq!(a.selected, a.rows.len() - 2);
    press(&mut a, "g");
    assert_eq!(a.selected, 0);

    // Number keys index the tab bar, whatever it is configured to hold.
    press(&mut a, "6");
    assert_eq!(a.view, View::Worktrees);
    press(&mut a, "1");
    assert_eq!(a.view, View::Ready);
    press(&mut a, "\t");
    assert_eq!(a.view, View::Mine);
    press(&mut a, "9"); // past the last tab: ignored
    assert_eq!(a.view, View::Mine);
}

#[test]
fn the_filter_narrows_as_you_type_and_esc_restores() {
    let mut a = sample_app();
    a.set_view(View::All);
    press(&mut a, "/tokens");
    assert_eq!(a.mode, Mode::Filter);
    assert_eq!(a.rows.len(), 1, "only the color-tokens PR matches");
    // While typing, letters are text, not commands: `q` must not quit.
    press(&mut a, "q");
    assert!(!a.quit && a.filter == "tokensq");
    press(&mut a, "\x08\n");
    assert!(
        a.mode != Mode::Filter && a.filter == "tokens",
        "enter keeps the filter"
    );
    press(&mut a, "\x1b");
    assert!(a.filter.is_empty());
    assert_eq!(a.rows.len(), 3);
}

#[test]
fn sort_drafts_and_help_toggle() {
    let mut a = sample_app();
    a.set_view(View::All);
    press(&mut a, "s");
    assert_eq!(a.sort, Sort::Attention);
    press(&mut a, "s");
    assert_eq!(a.sort, Sort::Recent);

    let with_drafts = a.rows.len();
    press(&mut a, "d");
    assert_eq!(a.rows.len(), with_drafts - 1, "the draft is hidden");
    press(&mut a, "d");
    assert_eq!(a.rows.len(), with_drafts);

    // `q` closes help first; only a second `q` quits.
    press(&mut a, "?");
    assert_eq!(a.mode, Mode::Help);
    press(&mut a, "j"); // other keys are swallowed while help is open
    assert_eq!(a.selected, 0);
    press(&mut a, "q");
    assert!(a.mode != Mode::Help && !a.quit);
    press(&mut a, "q");
    assert!(a.quit);
}

/// Every major state of the screen, pinned. A layout change shows up as a
/// reviewable diff in `cargo insta review` instead of relying on someone to
/// notice it by eye. Update deliberately: read the diff before accepting it.
#[test]
fn snapshots() {
    let mut a = sample_app();
    a.set_view(View::All);
    insta::assert_snapshot!("all_split_160x24", render(&mut a, 160, 24));

    a.settings.layout = LayoutMode::Stack;
    insta::assert_snapshot!("all_stacked_100x24", render(&mut a, 100, 24));
    a.settings.layout = LayoutMode::Auto;

    a.set_view(View::Worktrees);
    insta::assert_snapshot!("worktrees_120x18", render(&mut a, 120, 18));

    a.set_view(View::Mine);
    a.mode = Mode::Help;
    insta::assert_snapshot!("help_120x30", render(&mut a, 120, 30));
    a.mode = Mode::Browse;

    a.set_view(View::All);
    a.mode = Mode::Filter;
    a.filter = "retry".into();
    a.rebuild();
    insta::assert_snapshot!("filtering_120x12", render(&mut a, 120, 12));
    a.mode = Mode::Browse;
    a.filter = "zzz".into();
    a.rebuild();
    insta::assert_snapshot!("filter_matches_nothing_120x10", render(&mut a, 120, 10));
    a.filter.clear();
    a.rebuild();

    a.error = Some("gh api graphql: timed out after 45s".into());
    a.schedule.next_refresh = NOW + 180;
    insta::assert_snapshot!("sync_failed_120x10", render(&mut a, 120, 10));
    a.error = None;

    insta::assert_snapshot!("narrow_60x16", render(&mut a, 60, 16));
}

// ------------------------------------------------------------- worktrees

fn select_worktree(a: &mut App, name: &str) {
    a.set_view(View::Worktrees);
    let at = a
        .rows
        .iter()
        .position(|r| matches!(r, crate::app::Row::Wt(i) if a.worktrees[*i].name() == name))
        .unwrap();
    a.select(at);
}

fn worktree_mut<'a>(a: &'a mut App, name: &str) -> &'a mut Worktree {
    a.worktrees.iter_mut().find(|w| w.name() == name).unwrap()
}

/// After a squash merge whose remote branch was deleted and pruned, git has
/// no upstream to compare with and counts every commit on the branch as
/// unpushed. They are not: they are the PR that merged.
#[test]
fn a_squash_merged_desk_with_its_remote_branch_gone_is_removable() {
    let mut a = sample_app();
    let st = worktree_mut(&mut a, "drop-mono").status.as_mut().unwrap();
    st.published = false;
    st.unpushed = 2;
    let w = a
        .worktrees
        .iter()
        .find(|w| w.name() == "drop-mono")
        .unwrap();
    assert_eq!(a.wt_state(w), WtState::Removable);

    select_worktree(&mut a, "drop-mono");
    let out = render(&mut a, 160, 24);
    assert!(
        !out.contains("⇡2"),
        "landed commits shown as unpushed:\n{out}"
    );
    assert!(!out.contains("not on origin"), "{out}");
    assert!(
        out.contains("git worktree remove ~/src/widget-wt/drop-mono"),
        "{out}"
    );
}

/// Branch names are reused. A desk on a name that merged before, holding new
/// commits that never did, must not be offered for removal.
#[test]
fn a_reused_branch_name_is_not_a_landed_desk() {
    let mut a = sample_app();
    worktree_mut(&mut a, "drop-mono").head = "9a8b7c6d5e4f3021".into();
    let w = a
        .worktrees
        .iter()
        .find(|w| w.name() == "drop-mono")
        .unwrap();
    assert_eq!(a.wt_state(w), WtState::Idle);
    assert_eq!(a.removable_count(), 0);

    select_worktree(&mut a, "drop-mono");
    let out = render(&mut a, 160, 24);
    assert!(!out.contains("git worktree remove"), "{out}");
    assert!(out.contains("merged, but not at this commit"), "{out}");
}

/// A desk nobody has scanned has said nothing about local work, so it is
/// neither clean nor removable, and the screen does not claim either.
#[test]
fn an_unscanned_desk_makes_no_claims() {
    let mut a = sample_app();
    worktree_mut(&mut a, "drop-mono").status = None;
    let w = a
        .worktrees
        .iter()
        .find(|w| w.name() == "drop-mono")
        .unwrap();
    assert_eq!(a.wt_state(w), WtState::Idle);

    select_worktree(&mut a, "drop-mono");
    let out = render(&mut a, 160, 24);
    // Side by side, the detail pane shares screen lines with the list; read
    // only the list's columns.
    let list_width = usize::from(a.hits.list.width);
    let row = out
        .lines()
        .map(|l| l.chars().take(list_width).collect::<String>())
        .find(|l| l.contains("drop-mono"))
        .unwrap();
    assert!(
        !row.contains("local") && !row.contains("removable"),
        "{row}"
    );
    assert!(out.contains("not checked: worktree_status is off"), "{out}");
    assert!(!out.contains("clean"), "{out}");
}

#[test]
fn cleanup_commands_quote_paths_that_need_it() {
    let home = Some("/home/dev");
    let at = |path: &str| Worktree {
        path: path.into(),
        branch: None,
        head: String::new(),
        is_main: false,
        detached: false,
        status: None,
    };
    let shell = |path: &str| super::detail::shell_path(&at(path), home);
    assert_eq!(shell("/home/dev/src/wt/drop-mono"), "~/src/wt/drop-mono");
    // Quoted, the full path: the shell expands `~` only outside quotes.
    assert_eq!(shell("/home/dev/my desk"), "'/home/dev/my desk'");
    assert_eq!(shell("/home/dev/it's"), r"'/home/dev/it'\''s'");
}

// ------------------------------------------------------------ counts, input

/// A tab's count is the size of its list. Hidden drafts leave both.
#[test]
fn tab_counts_match_the_list_when_drafts_are_hidden() {
    let mut a = sample_app();
    press(&mut a, "d");
    for view in [View::All, View::Review, View::Mine] {
        a.set_view(view);
        assert_eq!(a.count_for(view), a.rows.len(), "{view:?}");
    }
    assert_eq!(
        a.count_for(View::Review),
        0,
        "the only review request is a draft"
    );
}

#[test]
fn a_ctrl_chord_types_nothing_into_the_filter() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut a = sample_app();
    press(&mut a, "/");
    crate::event::key(
        &mut a,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
    );
    assert!(a.filter.is_empty(), "{:?}", a.filter);
}

fn mouse(a: &mut App, kind: ratatui::crossterm::event::MouseEventKind, x: u16, y: u16) {
    let event = ratatui::crossterm::event::MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
    };
    crate::event::Input::default().mouse(a, event);
}

/// Side by side, the list and the detail pane share screen lines. A click in
/// the detail pane belongs to the detail pane.
#[test]
fn a_click_in_the_detail_pane_does_not_select_a_list_row() {
    use ratatui::crossterm::event::{MouseButton, MouseEventKind};
    let mut a = sample_app();
    a.set_view(View::All);
    a.settings.layout = LayoutMode::Split;
    render(&mut a, 160, 24);
    let (y, row) = a.hits.rows[1];
    let detail_x = a.hits.detail.x + a.hits.detail.width - 2;
    mouse(&mut a, MouseEventKind::Down(MouseButton::Left), detail_x, y);
    assert_eq!(a.selected, 0, "a detail-pane click selected row {row}");
    let list_x = a.hits.list.x + 4;
    mouse(&mut a, MouseEventKind::Down(MouseButton::Left), list_x, y);
    assert_eq!(a.selected, row);
}

/// Help is modal for the mouse as it is for keys.
#[test]
fn help_swallows_the_scroll_wheel_and_a_click_closes_it() {
    use ratatui::crossterm::event::{MouseButton, MouseEventKind};
    let mut a = sample_app();
    a.set_view(View::All);
    render(&mut a, 120, 24);
    a.mode = Mode::Help;
    mouse(&mut a, MouseEventKind::ScrollDown, 10, 5);
    assert_eq!(a.selected, 0, "scrolling moved the list behind help");
    assert_eq!(a.mode, Mode::Help);
    mouse(&mut a, MouseEventKind::Down(MouseButton::Left), 10, 5);
    assert_eq!(a.mode, Mode::Browse);
}

#[test]
fn a_failed_copy_does_not_claim_success() {
    let mut a = sample_app();
    a.set_view(View::All);
    a.settings.copy_command = "false".into();
    a.copy_url();
    let note = a.notice.clone().unwrap().0;
    assert!(note.contains("nothing copied"), "{note}");

    a.settings.copy_command = "true".into();
    a.copy_url();
    let note = a.notice.clone().unwrap().0;
    assert!(note.starts_with("copied "), "{note}");
}

// ----------------------------------------------------------------- drafts

fn draft_json(number: u64, edit: impl FnOnce(&mut serde_json::Value)) -> String {
    let mut v = crate::drafts::tests::sample();
    v["number"] = number.into();
    edit(&mut v);
    serde_json::to_string_pretty(&v).unwrap()
}

/// `sample_app` on the Drafts view, with a drafts directory holding one draft
/// in every state the view can show. The directory lives as long as the
/// returned guard.
fn drafts_app() -> (App, tempfile::TempDir) {
    let (a, _replies, dir) = drafts_app_and_replies();
    (a, dir)
}

fn drafts_app_and_replies() -> (
    App,
    std::sync::mpsc::Receiver<crate::app::Msg>,
    tempfile::TempDir,
) {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("acme").join("widget");
    std::fs::create_dir_all(&repo).unwrap();
    let put = |number: u64, text: String| {
        std::fs::write(repo.join(format!("{number}.json")), text).unwrap();
    };

    // Ready: a bare approval of the commit the PR is at.
    put(
        4846,
        draft_json(4846, |v| {
            v["body"] = json!("");
            v["comments"] = json!([]);
            v["note"] = json!("");
            v["picked"] = json!("Small, green and waiting on one review.");
        }),
    );
    // Ready, and saying more than a verdict.
    put(
        4840,
        draft_json(4840, |v| {
            v["verdict"] = json!("comment");
            v["body"] =
                json!("Reads well overall.\n\nOne question on the cap before this goes in.");
            v["comments"] = json!([
                { "path": "src/retry.rs", "line": 84, "side": "RIGHT",
                  "body": "Should this cap at the caller's deadline?" },
                { "path": "src/retry.rs", "line": 120, "side": "RIGHT",
                  "body": "A test for the zero-budget case would pin this." }
            ]);
            v["note"] = json!("Second look at the backoff math if there is time.");
            v["picked"] = json!("Touches the read path; small and self-contained.");
        }),
    );
    put(
        4801,
        draft_json(4801, |v| {
            v["status"] = json!("posted");
            // Half an hour after it was written, half an hour before now.
            v["posted_at"] = json!("2027-01-15T07:30:00Z");
        }),
    );
    put(4701, draft_json(4701, |v| v["status"] = json!("skipped")));
    // Merged at another commit than the draft was written against: stale.
    put(4700, draft_json(4700, |_| {}));
    // Not among the fetched pull requests: unverified.
    put(4990, draft_json(4990, |_| {}));
    put(4980, "{ \"version\": 1,".into());

    let (mut a, replies) = sample_app_and_replies();
    a.settings.views.push(View::Drafts);
    a.drafts_dir = Some(dir.path().to_path_buf());
    a.reload_drafts();
    a.set_view(View::Drafts);
    (a, replies, dir)
}

fn draft_numbers(a: &App) -> Vec<u64> {
    a.rows
        .iter()
        .map(|r| match r {
            // A file with no number in its name stands as 0.
            crate::app::Row::Draft(i) => a.drafts[*i].number.unwrap_or(0),
            crate::app::Row::Pr(_) | crate::app::Row::Wt(_) => {
                unreachable!("only drafts in this view")
            }
        })
        .collect()
}

fn select_draft(a: &mut App, number: u64) {
    let at = draft_numbers(a).iter().position(|n| *n == number).unwrap();
    a.select(at);
}

/// The list's own columns of the row for `number`: side by side, the detail
/// pane shares its screen line.
fn draft_row(a: &mut App, number: u64) -> String {
    let out = render(a, 160, 24);
    let list_width = usize::from(a.hits.list.width);
    out.lines()
        .map(|l| l.chars().take(list_width).collect::<String>())
        .find(|l| l.contains(&format!("#{number} ")))
        .unwrap_or_else(|| panic!("no row for #{number}:\n{out}"))
}

/// The state column is what the view is for: what posting would send, or the
/// one word for why nothing can be.
#[test]
fn each_draft_shows_what_it_would_send_or_why_it_cannot() {
    let (mut a, _dir) = drafts_app();
    for (number, state) in [
        (4846, " approve "),
        (4840, " comment +2 "),
        (4801, " ✓ posted "),
        (4701, " skipped "),
        (4700, " stale "),
        (4990, " unverified "),
        (4980, " invalid "),
    ] {
        let row = draft_row(&mut a, number);
        assert!(row.contains(state), "#{number}: {row:?}");
    }
    assert_eq!(a.count_for(View::Drafts), a.rows.len());

    // Title, size and age are the pull request's own.
    let row = draft_row(&mut a, 4846);
    assert!(
        row.contains("Give the read path a retry budget")
            && row.contains("+220 −24")
            && row.trim_end().ends_with("3m"),
        "{row:?}"
    );
    // A merged PR has a title and an age; one never fetched has neither.
    let row = draft_row(&mut a, 4700);
    assert!(
        row.contains("merged · Drop the monospace count chips") && row.trim_end().ends_with("4d"),
        "{row:?}"
    );
    let row = draft_row(&mut a, 4990);
    assert!(
        row.contains("not among the fetched pull requests"),
        "{row:?}"
    );
}

/// A draft carrying inline comments stands out; one already decided recedes.
#[test]
fn drafts_with_comments_stand_out_and_decided_ones_recede() {
    let (mut a, _dir) = drafts_app();
    // Keep the selection, which lifts every dim column, off the rows read.
    select_draft(&mut a, 4990);
    let buf = frame(&mut a, 160, 24);
    let y_of = |n: u64| {
        (0..24)
            .find(|y| row_text(&buf, *y).contains(&format!("#{n} ")))
            .unwrap()
    };
    // Column 1 is the number, column 8 the state.
    let fg = |n: u64, x: u16| buf[(x, y_of(n))].fg;
    assert_eq!(fg(4840, 8), a.theme.pending, "comments take the busy color");
    assert_eq!(fg(4846, 8), a.theme.success, "a bare approval");
    assert_eq!(fg(4700, 8), a.theme.warn, "stale");
    assert_eq!(fg(4980, 8), a.theme.failure, "invalid");
    for decided in [4801, 4701] {
        assert_eq!(fg(decided, 1), a.theme.muted, "#{decided} number");
        assert_eq!(fg(decided, 8), a.theme.muted, "#{decided} state");
    }
    assert_eq!(
        fg(4846, 1),
        a.theme.accent,
        "an undecided number stays bright"
    );
}

/// The detail pane is where a draft is read before it is sent: everything it
/// says, the note unmistakably private, and whether it still matches the PR.
#[test]
fn the_draft_detail_shows_the_whole_draft() {
    let (mut a, _dir) = drafts_app();
    select_draft(&mut a, 4840);
    let out = render(&mut a, 160, 30);
    for needle in [
        "#4840 Move the palette onto color tokens",
        "picked    Touches the read path; small and self-contained.",
        "verdict   comment",
        "body      Reads well overall.",
        "One question on the cap before this goes in.",
        "comment   src/retry.rs:84",
        "Should this cap at the caller's deadline?",
        "comment   src/retry.rs:120",
        "note      private · never posted",
        "Second look at the backoff math if there is time.",
        "head      1a2b3c4  matches the pull request",
    ] {
        assert!(out.contains(needle), "missing {needle:?}:\n{out}");
    }

    // An approval with no words says so rather than leaving a gap.
    select_draft(&mut a, 4846);
    let out = render(&mut a, 160, 30);
    assert!(
        out.contains("body      none") && out.contains("note      none"),
        "{out}"
    );

    select_draft(&mut a, 4801);
    let out = render(&mut a, 160, 30);
    assert!(out.contains("verdict   approve  · posted 30m ago"), "{out}");
}

/// The rows that cannot be posted say why, in the detail pane.
#[test]
fn the_draft_detail_explains_stale_unverified_and_invalid() {
    let (mut a, _dir) = drafts_app();

    select_draft(&mut a, 4700);
    let out = render(&mut a, 160, 30);
    assert!(
        out.contains("head      1a2b3c4  the pull request is now at 0f1e2d3"),
        "{out}"
    );
    assert!(out.contains("merged 4d ago"), "{out}");

    select_draft(&mut a, 4990);
    let out = render(&mut a, 160, 30);
    assert!(
        out.contains("head      1a2b3c4  the pull request's head is not known"),
        "{out}"
    );

    select_draft(&mut a, 4980);
    let out = render(&mut a, 160, 30);
    assert!(out.contains("invalid   not valid JSON"), "{out}");
    assert!(out.contains("4980.json"), "the file to fix: {out}");
    assert!(
        !out.contains("verdict"),
        "an unread draft has no verdict:\n{out}"
    );
}

/// Drafts order like every other view: newest pull request first, or by what
/// can be acted on. One whose pull request was never fetched goes last.
#[test]
fn drafts_sort_by_recency_or_by_what_can_be_posted() {
    let (mut a, _dir) = drafts_app();
    assert_eq!(
        draft_numbers(&a),
        [4846, 4840, 4801, 4700, 4701, 4980, 4990]
    );
    press(&mut a, "s");
    assert_eq!(
        draft_numbers(&a),
        [4846, 4840, 4700, 4990, 4980, 4701, 4801],
        "ready, stale, unverified, invalid, skipped, posted"
    );
    press(&mut a, "s/palette\n");
    assert_eq!(draft_numbers(&a), [4840], "the filter reads the PR's title");
}

/// The keys that act on a pull request act on a draft's pull request.
#[test]
fn open_on_a_draft_opens_its_pull_request() {
    let (mut a, _dir) = drafts_app();
    a.settings.open_command = "true".into();
    let mut opened = |number: u64| {
        select_draft(&mut a, number);
        press(&mut a, "o");
        a.notice.clone().unwrap().0
    };
    assert_eq!(
        opened(4846),
        "opened https://github.com/acme/widget/pull/4846"
    );
    assert_eq!(
        opened(4700),
        "opened https://github.com/acme/widget/pull/4700",
        "a merged pull request still has a page"
    );
    assert_eq!(
        opened(4990),
        "#4990 is not among the fetched pull requests",
        "an address is never made up"
    );
}

/// Drafts are opt-in, like Assigned: without the view on the bar and without
/// any drafts, nothing on the screen mentions them.
#[test]
fn drafts_stay_off_the_screen_until_asked_for() {
    assert!(!Settings::default().views.contains(&View::Drafts));
    assert_eq!(View::parse("drafts"), Some(View::Drafts));

    let mut a = sample_app();
    let out = render(&mut a, 160, 24);
    assert!(!out.contains("Drafts") && !out.contains("drafted"), "{out}");

    // With drafts waiting, every view says so, even with the tab off the bar.
    let (mut a, _dir) = drafts_app();
    a.settings.views.retain(|v| *v != View::Drafts);
    a.set_view(View::All);
    let subnav = row_text(&frame(&mut a, 160, 24), 1);
    assert!(subnav.contains("✎ 4 drafted"), "{subnav:?}");
    assert!(!subnav.contains("Drafts"), "{subnav:?}");

    // On a bar too tight for it the count goes first, and takes nothing else
    // with it.
    a.settings.views.push(View::Drafts);
    let subnav = row_text(&frame(&mut a, 110, 24), 1);
    assert!(
        subnav.contains("Drafts 7") && subnav.contains("↕ recent") && !subnav.contains("drafted"),
        "{subnav:?}"
    );
}

/// A drafts directory is read as it is on each refresh: a draft written
/// while rigor runs shows up, and one removed goes away.
#[test]
fn drafts_are_reread_on_refresh() {
    let (mut a, dir) = drafts_app();
    let repo = dir.path().join("acme").join("widget");
    std::fs::remove_file(repo.join("4990.json")).unwrap();
    std::fs::write(repo.join("4801.json"), "nonsense").unwrap();
    a.reload_drafts();
    assert_eq!(a.rows.len(), 6);
    assert!(draft_row(&mut a, 4801).contains(" invalid "));
}

/// A fresh app on the Drafts view of an empty drafts directory, and a way to
/// drop a valid draft into it. None of these pull requests were fetched, so
/// the rows keep the order of their numbers.
fn bare_drafts() -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("acme").join("widget")).unwrap();
    let mut a = sample_app();
    a.settings.views.push(View::Drafts);
    a.drafts_dir = Some(dir.path().to_path_buf());
    a.set_view(View::Drafts);
    (a, dir)
}

fn put_draft(dir: &tempfile::TempDir, number: u64) {
    let path = dir.path().join(format!("acme/widget/{number}.json"));
    std::fs::write(path, draft_json(number, |_| {})).unwrap();
}

fn cursor(a: &App) -> Option<u64> {
    a.selected_draft().and_then(|e| e.number)
}

/// The cursor is on a draft, not on a position. Drafts arrive while rigor is
/// open; if one sorting earlier could slide the highlight onto its neighbor,
/// the next keypress would be about a draft nobody chose.
#[test]
fn the_cursor_stays_on_its_draft_when_the_list_changes_underneath() {
    // Whether or not the cursor was ever moved by hand.
    for touched in [false, true] {
        let (mut a, dir) = bare_drafts();
        put_draft(&dir, 100);
        put_draft(&dir, 105);
        a.reload_drafts();
        assert_eq!(draft_numbers(&a), [100, 105]);
        if touched {
            a.select(0);
        }
        assert_eq!(cursor(&a), Some(100));
        a.detail_scroll = 3;

        put_draft(&dir, 90);
        a.reload_drafts();
        assert_eq!(draft_numbers(&a), [90, 100, 105]);
        assert_eq!(cursor(&a), Some(100), "touched={touched}: the cursor slid");
        assert_eq!(a.detail_scroll, 3, "and its detail pane kept its place");

        // One going away above it moves the row, not the choice.
        std::fs::remove_file(dir.path().join("acme/widget/90.json")).unwrap();
        a.reload_drafts();
        assert_eq!(cursor(&a), Some(100));

        // Its own file going away leaves the cursor where it was in the list,
        // on whatever is nearest, and at the top of that draft.
        put_draft(&dir, 90);
        a.reload_drafts();
        std::fs::remove_file(dir.path().join("acme/widget/100.json")).unwrap();
        a.reload_drafts();
        assert_eq!(draft_numbers(&a), [90, 105]);
        assert_eq!(cursor(&a), Some(105), "the nearest row, not the first");
        assert_eq!(a.detail_scroll, 0);
    }
}

/// A pull request number of any length is shown whole, and the columns
/// beside it still line up.
#[test]
fn long_pull_request_numbers_are_not_cut_in_the_drafts_list() {
    let (mut a, dir) = bare_drafts();
    for number in [7, 123_456, 1_234_567] {
        put_draft(&dir, number);
    }
    a.reload_drafts();
    let screen = render(&mut a, 160, 24);
    let row = |number: u64| {
        screen
            .lines()
            .find(|l| l.contains(&format!("#{number} ")))
            .unwrap_or_else(|| panic!("#{number} is cut or missing:\n{screen}"))
            .to_string()
    };
    // In columns, not bytes: the selected row starts with a wider character.
    let column = |number: u64| {
        let row = row(number);
        row.split("unverified").next().unwrap().chars().count()
    };
    assert!(row(1_234_567).contains("#1234567 unverified"), "{screen}");
    assert_eq!(column(7), column(123_456), "{screen}");
    assert_eq!(column(7), column(1_234_567), "{screen}");
    assert!(!screen.contains("#12345…") && !screen.contains("#1234…"));
}

/// A fetch re-sorts the list by how recently each pull request moved. The
/// cursor goes with its draft there too.
#[test]
fn the_cursor_stays_on_its_draft_when_a_fetch_reorders_the_list() {
    let (mut a, dir) = bare_drafts();
    put_draft(&dir, 100);
    put_draft(&dir, 105);
    a.reload_drafts();
    assert_eq!(cursor(&a), Some(100));

    // #105 turns out to be an open pull request; #100 is still unknown, and
    // sorts after it.
    let mut seen = a.prs[0].clone();
    seen.number = 105;
    a.on_msg(crate::app::Msg::Prs(Ok(crate::github::Fetched {
        budget: None,
        viewer: "octocat".into(),
        default_branch: "main".into(),
        prs: vec![seen],
        merged: vec![],
    })));
    assert_eq!(draft_numbers(&a), [105, 100]);
    assert_eq!(cursor(&a), Some(100));
}

/// A draft longer than the pane says there is more of it, and a refresh in
/// the background does not throw the reader back to its top.
#[test]
fn a_long_draft_says_there_is_more_below_and_keeps_its_place() {
    let (mut a, dir) = drafts_app();
    let long = draft_json(4846, |v| {
        v["body"] = (1..=40)
            .map(|i| format!("Paragraph {i} of the review."))
            .collect::<Vec<_>>()
            .join("\n")
            .into();
    });
    std::fs::write(dir.path().join("acme/widget/4846.json"), long).unwrap();
    a.reload_drafts();
    select_draft(&mut a, 4846);

    let out = render(&mut a, 160, 24);
    assert!(out.contains("↓ more below · J"), "{out}");
    assert!(!out.contains("Paragraph 40"), "{out}");

    press(&mut a, "JJJJJ");
    a.reload_drafts();
    a.on_msg(crate::app::Msg::Worktrees(Ok(a.worktrees.clone())));
    assert_eq!(a.detail_scroll, 5, "a background refresh scrolled it back");

    // At the end there is no more, and the pane stops saying so.
    a.detail_scroll = u16::MAX;
    let out = render(&mut a, 160, 24);
    assert!(
        out.contains("Paragraph 40") && !out.contains("more below"),
        "{out}"
    );

    // A short draft never says it.
    select_draft(&mut a, 4701);
    assert!(!render(&mut a, 160, 24).contains("more below"));
}

/// Nothing overflows the frame, at any width, with every state on screen.
#[test]
fn drafts_render_at_every_width() {
    let (mut a, _dir) = drafts_app();
    for number in [4840, 4980] {
        select_draft(&mut a, number);
        for (w, h) in [
            (200u16, 40u16),
            (140, 30),
            (110, 24),
            (80, 20),
            (52, 12),
            (40, 8),
        ] {
            let out = render(&mut a, w, h);
            assert!(
                out.lines().all(|l| crate::util::width(l) <= w as usize),
                "#{number} overflowed at {w}x{h}:\n{out}"
            );
        }
    }
}

#[test]
fn drafts_snapshots() {
    let (mut a, _dir) = drafts_app();
    select_draft(&mut a, 4840);
    insta::assert_snapshot!("drafts_split_160x24", render(&mut a, 160, 24));

    a.settings.layout = LayoutMode::Stack;
    select_draft(&mut a, 4846);
    insta::assert_snapshot!("drafts_stacked_100x24", render(&mut a, 100, 24));
    a.settings.layout = LayoutMode::Auto;

    insta::assert_snapshot!("drafts_narrow_60x16", render(&mut a, 60, 16));
}

proptest::proptest! {
    /// A draft is a file some other program wrote. Its text is drawn into the
    /// terminal like a title from the API, and gets the same guarantee.
    #[test]
    fn a_drafts_text_never_reaches_the_terminal_as_control_characters(
        payload in proptest::string::string_regex(
            "[a-z ]{0,6}(\\x1b\\[31m|\\x1b\\]8;;https://evil\\x07|\\x07|\\x08|\\r|\\x9b2J|\\u{202e}|\\u{2067}|\\x00)[a-z ]{0,6}"
        ).unwrap()
    ) {
        let (mut a, dir) = drafts_app();
        let text = draft_json(4846, |v| {
            v["body"] = format!("BODYTEXT{payload}").into();
            v["note"] = format!("NOTETEXT{payload}").into();
            v["picked"] = format!("PICKEDTEXT{payload}").into();
            v["comments"][0]["path"] = format!("PATHTEXT{payload}").into();
            v["comments"][0]["body"] = format!("COMMENTTEXT{payload}").into();
        });
        std::fs::write(dir.path().join("acme/widget/4846.json"), text).unwrap();
        a.reload_drafts();
        select_draft(&mut a, 4846);
        // The draft was drawn at all: without this, a pane that drew nothing
        // would pass for one that drew nothing dangerous.
        let drawn = render(&mut a, 160, 30);
        for printable in ["BODYTEXT", "NOTETEXT", "PICKEDTEXT", "PATHTEXT", "COMMENTTEXT"] {
            proptest::prop_assert!(drawn.contains(printable), "{} was not drawn:\n{}", printable, drawn);
        }
        let buf = frame(&mut a, 160, 30);
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let sym = buf[(x, y)].symbol();
                let dangerous = |c: char| {
                    c.is_control()
                        || ('\u{202a}'..='\u{202e}').contains(&c)
                        || ('\u{2066}'..='\u{2069}').contains(&c)
                };
                proptest::prop_assert!(
                    !sym.chars().any(dangerous),
                    "dangerous character {:?} at ({x},{y}) from payload {:?}", sym, payload
                );
            }
        }
    }
}

// ---------------------------------------------------------------- posting

use crate::drafts::{Comment, Live, PostFailure, Remote, Review, Side};
use std::sync::{Arc, Mutex};

/// The commit a pull request has moved on to, in the tests where it has.
const MOVED: &str = "9f8e7d6c5b4a39281706f5e4d3c2b1a098765432";

/// The frozen clock, as GitHub would stamp a review submitted at it: the time
/// the app itself would have posted. The drafts were written an hour before.
const POSTED_NOW: &str = "2027-01-15T08:00:00Z";

/// GitHub, for these tests: it answers from what it has been told, records
/// every call in the order it came, and reaches nothing. An `App` only talks
/// to GitHub through the remote it is given, so no test here can run `gh`.
struct FakeGitHub {
    calls: Mutex<Vec<String>>,
    /// Pull requests with something other than the default to say about
    /// themselves. The default is open, at the commit the drafts were
    /// written against.
    pulls: Mutex<std::collections::HashMap<u64, Result<Live, String>>>,
    reviews: Mutex<Result<Vec<Review>, String>>,
    /// The inline comments of review 77, the one `review_by_viewer` makes.
    comments: Mutex<Result<Vec<Comment>, String>>,
    /// `Ok(None)` answers a post as GitHub would: with the review the request
    /// describes, by the viewer.
    post: Mutex<Result<Option<Review>, PostFailure>>,
    /// Run when a post arrives, before it is answered.
    on_post: Mutex<Option<OnPost>>,
}

type OnPost = Box<dyn Fn(&FakeGitHub) + Send>;

impl FakeGitHub {
    fn pull_is(&self, number: u64, live: Result<Live, String>) {
        self.pulls.lock().unwrap().insert(number, live);
    }

    fn reviews_are(&self, reviews: Result<Vec<Review>, String>) {
        *self.reviews.lock().unwrap() = reviews;
    }

    fn comments_are(&self, comments: Result<Vec<Comment>, String>) {
        *self.comments.lock().unwrap() = comments;
    }

    fn post_answers(&self, answer: Result<Option<Review>, PostFailure>) {
        *self.post.lock().unwrap() = answer;
    }

    fn on_post(&self, f: impl Fn(&Self) + Send + 'static) {
        *self.on_post.lock().unwrap() = Some(Box::new(f));
    }

    /// The review the fixture's draft for #4840 would be, landed: as GitHub
    /// would list it had the app posted it just now.
    fn has_4840s_review(&self) {
        self.reviews_are(Ok(vec![review_by_viewer("COMMENTED", BODY_4840)]));
        self.comments_are(Ok(comments_4840()));
    }
}

impl Remote for FakeGitHub {
    fn pull(&self, number: u64) -> Result<Live, String> {
        self.calls.lock().unwrap().push(format!("pull {number}"));
        self.pulls
            .lock()
            .unwrap()
            .get(&number)
            .cloned()
            .unwrap_or_else(|| Ok(live(true, false, crate::drafts::tests::HEAD)))
    }

    // Every list here fits on its first page.
    fn reviews(&self, number: u64, page: usize) -> Result<Vec<Review>, String> {
        assert_eq!(page, 1, "a second page of a short list was asked for");
        self.calls.lock().unwrap().push(format!("reviews {number}"));
        self.reviews.lock().unwrap().clone()
    }

    fn comments(&self, number: u64, page: usize) -> Result<Vec<crate::drafts::Placed>, String> {
        assert_eq!(page, 1, "a second page of a short list was asked for");
        self.calls
            .lock()
            .unwrap()
            .push(format!("comments {number}"));
        let of_77 = self.comments.lock().unwrap().clone()?;
        Ok(of_77
            .into_iter()
            .map(|at| crate::drafts::Placed {
                review: Some(77),
                at: Some(at),
            })
            .collect())
    }

    fn post(&self, number: u64, body: &str) -> Result<Review, PostFailure> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("post {number} {body}"));
        if let Some(f) = self.on_post.lock().unwrap().as_ref() {
            f(self);
        }
        let answer = self.post.lock().unwrap().clone()?;
        Ok(answer.unwrap_or_else(|| crate::drafts::tests::created_by("octocat", body)))
    }

    fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(45)
    }
}

fn live(open: bool, merged: bool, head: &str) -> Live {
    Live {
        open,
        merged,
        head: head.into(),
    }
}

/// A review by the viewer on the commit the drafts were written against,
/// submitted at the frozen clock.
fn review_by_viewer(state: &str, body: &str) -> Review {
    Review {
        id: 77,
        author: "octocat".into(),
        state: state.into(),
        commit: crate::drafts::tests::HEAD.into(),
        body: body.into(),
        submitted_at: Some(POSTED_NOW.into()),
    }
}

/// The body of the fixture's draft for #4840, as it would be sent.
const BODY_4840: &str = "Reads well overall.\n\nOne question on the cap before this goes in.";

/// And its two inline comments.
fn comments_4840() -> Vec<Comment> {
    vec![
        Comment {
            path: "src/retry.rs".into(),
            line: 84,
            side: Side::Right,
            body: "Should this cap at the caller's deadline?".into(),
        },
        Comment {
            path: "src/retry.rs".into(),
            line: 120,
            side: Side::Right,
            body: "A test for the zero-budget case would pin this.".into(),
        },
    ]
}

/// The drafts fixture wired to `FakeGitHub`, on a clock the test moves.
struct Outbox {
    a: App,
    gh: Arc<FakeGitHub>,
    replies: std::sync::mpsc::Receiver<crate::app::Msg>,
    dir: tempfile::TempDir,
    /// What `now_millis` says. Nothing here sleeps.
    now: i64,
}

impl Outbox {
    fn new() -> Self {
        let (mut a, replies, dir) = drafts_app_and_replies();
        let gh = Arc::new(FakeGitHub {
            calls: Mutex::default(),
            pulls: Mutex::default(),
            reviews: Mutex::new(Ok(Vec::new())),
            comments: Mutex::new(Ok(Vec::new())),
            post: Mutex::new(Ok(None)),
            on_post: Mutex::new(None),
        });
        a.remote = Arc::clone(&gh) as Arc<dyn Remote>;
        // Nothing here opens a browser or touches the clipboard, whatever
        // key a test presses.
        a.settings.open_command = "true".into();
        a.settings.copy_command = "true".into();
        crate::util::freeze_millis(0);
        Self {
            a,
            gh,
            replies,
            dir,
            now: 0,
        }
    }

    /// Let this long go by.
    fn wait(&mut self, millis: i64) {
        self.now += millis;
        crate::util::freeze_millis(self.now);
    }

    /// Every call GitHub was asked to take, in order.
    fn calls(&self) -> Vec<String> {
        self.gh.calls.lock().unwrap().clone()
    }

    /// How many of those were writes.
    fn posts(&self) -> usize {
        self.calls()
            .iter()
            .filter(|c| c.starts_with("post "))
            .count()
    }

    /// The first `a` on `number`, and the frame that follows it: the
    /// question is up and has been drawn, as it would be before any second
    /// key could arrive.
    fn ask(&mut self, number: u64) {
        select_draft(&mut self.a, number);
        self.wait(1_000);
        press(&mut self.a, "a");
        render(&mut self.a, 160, 40);
    }

    /// `y`, a deliberate while after the question was drawn.
    fn answer(&mut self) {
        self.wait(1_000);
        press(&mut self.a, "y");
    }

    /// Ask, answer, and take the reply, as the event loop would deliver it.
    fn confirm(&mut self, number: u64) {
        self.ask(number);
        self.answer();
        self.settle();
    }

    /// Let the attempt in flight report back.
    fn settle(&mut self) {
        let reply = self
            .replies
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the post never reported back");
        self.a.on_msg(reply);
    }

    fn path(&self, number: u64) -> std::path::PathBuf {
        self.dir
            .path()
            .join("acme")
            .join("widget")
            .join(format!("{number}.json"))
    }

    fn file(&self, number: u64) -> String {
        std::fs::read_to_string(self.path(number)).unwrap()
    }

    fn json(&self, number: u64) -> serde_json::Value {
        serde_json::from_str(&self.file(number)).unwrap()
    }

    fn failure(&self) -> String {
        self.a.post_error.clone().expect("no failure was reported")
    }

    fn note(&self) -> String {
        self.a.notice.clone().expect("nothing was noted").0
    }
}

/// One key event of a given kind: a press, a repeat from a held key, or a
/// release, as a terminal that reports them would send it.
fn key_of(a: &mut App, ch: char, kind: ratatui::crossterm::event::KeyEventKind) {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventState, KeyModifiers};
    crate::event::key(
        a,
        KeyEvent {
            code: KeyCode::Char(ch),
            modifiers: KeyModifiers::NONE,
            kind,
            state: KeyEventState::NONE,
        },
    );
}

/// The exact JSON the fixture's draft for #4840 is sent as.
fn request_4840() -> String {
    let head = crate::drafts::tests::HEAD;
    format!(
        r#"{{"commit_id":"{head}","event":"COMMENT","body":"Reads well overall.\n\nOne question on the cap before this goes in.","comments":[{{"path":"src/retry.rs","line":84,"side":"RIGHT","body":"Should this cap at the caller's deadline?"}},{{"path":"src/retry.rs","line":120,"side":"RIGHT","body":"A test for the zero-budget case would pin this."}}]}}"#
    )
}

/// Posting is public and under the user's name. One `a` only asks, showing
/// exactly what would be sent. A second, deliberate `a` asks GitHub where the
/// pull request is and what has been said on it, then sends the review once,
/// and only then is the file marked posted.
#[test]
fn a_asks_and_y_checks_then_posts_exactly_the_review() {
    let mut o = Outbox::new();
    let before = o.json(4840);

    o.ask(4840);
    assert!(
        o.a.posting.is_none() && o.calls().is_empty(),
        "one a reached GitHub: {:?}",
        o.calls()
    );
    let prompt = footer(&frame(&mut o.a, 160, 40));
    assert!(
        prompt.contains("post comment with 2 inline comments to #4840? y to post, esc to cancel"),
        "{prompt:?}"
    );
    assert_eq!(o.file(4840), serde_json::to_string_pretty(&before).unwrap());

    o.answer();
    assert!(
        draft_row(&mut o.a, 4840).contains(" posting… "),
        "the row being sent says so"
    );
    o.settle();

    assert_eq!(
        o.calls(),
        [
            "pull 4840".to_string(),
            "reviews 4840".to_string(),
            format!("post 4840 {}", request_4840())
        ],
        "two reads, then exactly one write"
    );
    let body = request_4840();
    assert!(
        !body.contains("backoff math") && !body.contains("self-contained"),
        "note or picked left the machine: {body}"
    );

    // The file says posted, when, and nothing else about it changed.
    let mut want = before;
    want["status"] = "posted".into();
    want["posted_at"] = POSTED_NOW.into();
    assert_eq!(o.json(4840), want);
    assert!(draft_row(&mut o.a, 4840).contains(" ✓ posted "));
    assert!(o.a.posting.is_none() && o.a.post_error.is_none());
    assert_eq!(o.note(), "posted to #4840");
}

/// A failed sync holds the footer until the next good fetch. The question must
/// still be seen: a prompt nobody can read is a stray keypress from posting.
#[test]
fn the_prompt_outranks_a_sync_error() {
    let mut o = Outbox::new();
    o.a.error = Some("gh api graphql: timed out after 45s".into());
    o.ask(4846);
    let line = footer(&frame(&mut o.a, 160, 40));
    assert!(
        line.contains("post approve to #4846? y to post") && !line.contains("timed out"),
        "{line:?}"
    );
}

// ------------------------------------------------- the two presses, in time

/// A key held down repeats once the keyboard's own delay has passed — two
/// fifths of a second on one machine, most of a second on another — and then
/// every few hundredths. Nothing about the gap between two `a`s can tell
/// that first repeat from a second, deliberate press. So `a` is never the
/// answer: held for as long as you like, it opens the question and closes it
/// again, and posts nothing.
#[test]
fn a_held_a_never_posts() {
    use ratatui::crossterm::event::KeyEventKind::{Press, Repeat};
    for first_repeat in [400, 500, 700] {
        // Terminals that report a repeat as one, and those that send it as
        // another press.
        for kind in [Press, Repeat] {
            let mut o = Outbox::new();
            select_draft(&mut o.a, 4840);
            o.wait(1_000);
            press(&mut o.a, "a");
            render(&mut o.a, 160, 40);
            assert!(o.a.armed.is_some());

            o.wait(first_repeat);
            for _ in 0..80 {
                key_of(&mut o.a, 'a', kind);
                render(&mut o.a, 160, 40);
                assert!(
                    o.a.posting.is_none(),
                    "a held a posted ({kind:?}, first repeat after {first_repeat} ms)"
                );
                o.wait(30);
            }
            assert!(o.calls().is_empty(), "{:?}", o.calls());
        }
    }
}

/// The key that asks is not the key that answers. A second `a`, however
/// deliberate, takes the question down and does nothing else.
#[test]
fn a_then_a_cancels() {
    let mut o = Outbox::new();
    o.ask(4840);
    o.wait(5_000);
    press(&mut o.a, "a");
    assert!(o.a.armed.is_none(), "a second a left the question up");
    assert!(o.a.posting.is_none() && o.calls().is_empty());
}

/// `y` answers — once the question has been on screen long enough to have
/// been seen. The wait is counted from when the overlay was first drawn, not
/// from the key that asked for it: a repaint that stalls does not use it up.
#[test]
fn an_immediate_y_is_ignored_and_a_later_one_posts() {
    use crate::app::ANSWER_FLOOR_MS;
    let mut o = Outbox::new();
    select_draft(&mut o.a, 4840);
    o.wait(1_000);
    press(&mut o.a, "a");

    // The frame is slow to come. However long ago the `a` was, nothing has
    // been shown, so nothing can be answered.
    o.wait(5_000);
    press(&mut o.a, "y");
    assert!(o.a.posting.is_none(), "answered before it was drawn");
    assert!(o.a.armed.is_some(), "and the question is not taken down");

    render(&mut o.a, 160, 40);
    press(&mut o.a, "y");
    assert!(o.a.posting.is_none(), "answered in the instant it appeared");
    o.wait(ANSWER_FLOOR_MS - 1);
    render(&mut o.a, 160, 40);
    press(&mut o.a, "y");
    assert!(o.a.posting.is_none(), "one millisecond short is short");
    assert!(o.a.armed.is_some() && o.calls().is_empty());

    o.wait(1);
    press(&mut o.a, "y");
    assert!(o.a.posting.is_some(), "a y after the pause is the answer");
    o.settle();
    assert_eq!(o.posts(), 1);
}

/// Keys typed ahead arrive together. `ay` in one breath asks, and the `y` is
/// not an answer to a question that has only just appeared.
#[test]
fn typed_ahead_ay_does_not_post() {
    // Handed over before any frame is drawn.
    let mut o = Outbox::new();
    select_draft(&mut o.a, 4840);
    o.wait(1_000);
    press(&mut o.a, "ay");
    assert!(o.a.posting.is_none());

    // And as the event loop takes them: one key, one frame, the next key.
    let mut o = Outbox::new();
    select_draft(&mut o.a, 4840);
    o.wait(1_000);
    press(&mut o.a, "a");
    render(&mut o.a, 160, 40);
    press(&mut o.a, "y");
    render(&mut o.a, 160, 40);
    press(&mut o.a, "yyy");
    assert!(o.a.posting.is_none() && o.a.armed.is_some());
    assert!(o.calls().is_empty(), "{:?}", o.calls());
}
/// Pasted text is not typing. With bracketed paste it arrives as a paste,
/// which neither asks nor answers; without, it arrives as keys all at once,
/// and its `y` comes before the question has been on screen at all.
#[test]
fn pasted_text_never_asks_or_confirms() {
    // As a paste, with nothing asked: nothing is asked by it.
    let mut o = Outbox::new();
    select_draft(&mut o.a, 4840);
    crate::event::paste(&mut o.a, "ay");
    assert!(o.a.armed.is_none() && o.a.posting.is_none());

    // As a paste, with the question up and long enough ago to answer: it is
    // not an answer, and the question does not survive it.
    o.ask(4840);
    o.wait(5_000);
    crate::event::paste(&mut o.a, "y");
    assert!(o.a.armed.is_none() && o.a.posting.is_none());

    // As keys, in the same instant.
    o.wait(1_000);
    press(&mut o.a, "ay");
    render(&mut o.a, 160, 40);
    press(&mut o.a, "y");
    assert!(o.a.posting.is_none(), "pasted keys posted");
    assert!(o.calls().is_empty(), "{:?}", o.calls());

    // Where text is wanted, a paste is still text.
    press(&mut o.a, "\x1b/");
    crate::event::paste(&mut o.a, "pal\x1bette");
    assert_eq!(o.a.filter, "palette");
}
/// A question nobody answers or reads goes away on its own, so that a `y`
/// typed long after is not about it. Reading it — scrolling — gives it
/// longer. And it goes by the wall clock too: a question asked before the
/// lid closed, when the other clock stops, is gone when it opens.
#[test]
fn the_question_expires() {
    use crate::app::ARMED_FOR_MS;
    let mut o = Outbox::new();
    o.ask(4840);
    o.wait(ARMED_FOR_MS - 1);
    o.a.tick();
    assert!(o.a.armed.is_some(), "it went before its time");
    o.wait(1);
    o.a.tick();
    assert!(o.a.armed.is_none(), "it outlived its time");

    // A `y` now is not an answer to anything.
    o.wait(60_000);
    press(&mut o.a, "y");
    assert!(o.a.posting.is_none() && o.calls().is_empty());

    // Reading starts the thirty seconds again.
    o.ask(4840);
    o.wait(ARMED_FOR_MS - 5_000);
    press(&mut o.a, "j");
    o.wait(ARMED_FOR_MS - 5_000);
    o.a.tick();
    assert!(o.a.armed.is_some(), "it expired while it was being read");
    o.wait(5_000);
    o.a.tick();
    assert!(o.a.armed.is_none());

    // Asleep: the clock the floor is counted on has not moved at all, and
    // half a minute has gone by on the wall.
    o.ask(4840);
    crate::util::freeze_time(NOW + 29);
    o.a.tick();
    assert!(o.a.armed.is_some());
    crate::util::freeze_time(NOW + 30);
    o.a.tick();
    assert!(
        o.a.armed.is_none(),
        "it was still up after the machine woke"
    );
    crate::util::freeze_time(NOW);
}
// ------------------------------------------------------------- the overlay

fn overlay_lines(o: &Outbox, width: usize) -> Vec<String> {
    let armed = o.a.armed.as_ref().expect("no question is up");
    super::confirm::lines("#1 Title", &armed.draft, &o.a.theme, width)
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

/// The overlay is what will be posted: the pull request first, the verdict,
/// the words, each inline comment where it lands — and none of what stays
/// private.
#[test]
fn the_overlay_shows_what_will_be_posted_and_nothing_else() {
    let mut o = Outbox::new();
    let text = draft_json(4840, |v| {
        v["verdict"] = "comment".into();
        v["body"] = "Reads well overall.".into();
        v["comments"] = serde_json::json!([
            { "path": "src/retry.rs", "line": 84, "side": "RIGHT", "body": "Cap at the deadline?" },
            { "path": "src/old.rs", "line": 7, "side": "LEFT", "body": "Why was this dropped?" }
        ]);
        v["note"] = "NOTE-STAYS-HOME".into();
        v["picked"] = "PICKED-STAYS-HOME".into();
    });
    std::fs::write(o.path(4840), text).unwrap();
    o.a.reload_drafts();
    o.ask(4840);

    let lines = overlay_lines(&o, 80);
    assert_eq!(
        lines,
        [
            " #1 Title",
            "",
            "  verdict   comment",
            "  body      Reads well overall.",
            "  comment   src/retry.rs:84",
            "            Cap at the deadline?",
            "  comment   src/old.rs:7 (LEFT)",
            "            Why was this dropped?",
        ]
    );

    // On the screen it is headed by the real pull request, number first.
    let screen = render(&mut o.a, 160, 40);
    assert!(
        screen.contains("#4840 Move the palette onto color tokens"),
        "{screen}"
    );
    assert!(screen.contains("this is what will be posted"), "{screen}");
    assert!(screen.contains("y posts · esc cancels"), "{screen}");

    // An approval with no words says there are none, rather than nothing.
    o.a.armed = None;
    o.ask(4846);
    let lines = overlay_lines(&o, 80);
    assert_eq!(lines[2..], ["  verdict   approve", "  body      no text"]);
}

/// Nothing in the overlay is cut short. A path too long for the line wraps,
/// so the file name and the line number — the parts that say where a comment
/// will land — are always there to be read. So does a long word in a body.
#[test]
fn the_overlay_wraps_what_does_not_fit_and_cuts_nothing() {
    let path = "crates/engine/src/storage/backends/filesystem/layout/segments/\
                compaction/strategy/tiered/planner/the_file_that_matters.rs";
    let word = "x".repeat(150);
    let mut o = Outbox::new();
    let text = draft_json(4840, |v| {
        v["body"] = format!("See {word} above.").into();
        v["comments"] = serde_json::json!([
            { "path": path, "line": 1234, "side": "LEFT", "body": "This one." }
        ]);
    });
    std::fs::write(o.path(4840), text).unwrap();
    o.a.reload_drafts();
    o.ask(4840);

    for width in [82, 60, 40] {
        let lines = overlay_lines(&o, width);
        for l in &lines {
            assert!(crate::util::width(l) <= width, "{width}: too wide: {l:?}");
            assert!(!l.contains('…'), "{width}: cut short: {l:?}");
        }
        // Put back together, every character of where it lands is there.
        let values: String = lines
            .iter()
            .map(|l| l.chars().skip(12).collect::<String>())
            .collect();
        assert!(
            values.contains(&format!("{path}:1234 (LEFT)")),
            "{width}: {lines:#?}"
        );
        assert!(values.contains(&word), "{width}: {lines:#?}");
    }

    // On the screen too, and all of it counts as rows to be read.
    let screen = render(&mut o.a, 100, 40);
    assert!(
        screen.contains("the_file_that_matters.rs:1234 (LEFT)"),
        "{screen}"
    );
    assert!(
        !screen
            .lines()
            .filter(|l| l.contains('│'))
            .any(|l| l.contains('…')),
        "{screen}"
    );
}

/// Where a comment lands is broken between directories where it can be, and
/// inside a name only when the name alone is longer than the line.
#[test]
fn a_long_path_breaks_after_a_slash_where_it_can() {
    let wrapped = super::confirm::wrap_place("a/bb/ccc/dddd.rs:12", 6);
    assert_eq!(wrapped, ["a/bb/", "ccc/", "dddd.r", "s:12"]);
    assert_eq!(
        super::confirm::wrap_place("src/x.rs:84", 40),
        ["src/x.rs:84"]
    );
}

/// A draft of forty paragraphs and a comment after them, which no overlay
/// shows at once.
fn long_draft(o: &mut Outbox) {
    let text = draft_json(4840, |v| {
        v["body"] = (1..=40)
            .map(|i| format!("Paragraph {i} of the review."))
            .collect::<Vec<_>>()
            .join("\n")
            .into();
        v["comments"] = serde_json::json!([
            { "path": "src/last.rs", "line": 9, "side": "RIGHT", "body": "THE-LAST-LINE" }
        ]);
    });
    std::fs::write(o.path(4840), text).unwrap();
    o.a.reload_drafts();
}

/// Nothing is posted unread. A draft longer than the overlay scrolls inside
/// it, the scrolling leaves the question up, and `y` is turned away until
/// every row of it has been on screen — which jumping to the end is not.
#[test]
fn nothing_is_posted_until_every_row_has_been_on_screen() {
    let mut o = Outbox::new();
    long_draft(&mut o);
    select_draft(&mut o.a, 4840);
    o.wait(1_000);
    press(&mut o.a, "a");
    let screen = render(&mut o.a, 120, 20);
    assert!(
        screen.contains("Paragraph 1 ") && !screen.contains("THE-LAST-LINE"),
        "{screen}"
    );
    let unread = o.a.armed.as_ref().unwrap().unread();
    assert!(unread > 0);
    assert!(
        screen.contains(&format!("{unread} rows still to read")),
        "{screen}"
    );
    let prompt = footer(&frame(&mut o.a, 120, 20));
    assert!(
        prompt.contains("rows still to read") && !prompt.contains("y to post"),
        "{prompt:?}"
    );

    // However deliberate, `y` does not send what has not been shown.
    o.answer();
    assert!(o.a.posting.is_none(), "an unread draft was posted");
    assert!(o.a.armed.is_some(), "and the question is still there");

    // Straight to the end: the last row has been shown, the middle has not.
    press(&mut o.a, "G");
    let screen = render(&mut o.a, 120, 20);
    assert!(screen.contains("THE-LAST-LINE"), "{screen}");
    let skipped = o.a.armed.as_ref().unwrap().unread();
    assert!(
        skipped > 0 && skipped < unread,
        "the end was taken for the whole: {skipped} of {unread}"
    );
    assert!(
        screen.contains(&format!("{skipped} rows still to read")),
        "{screen}"
    );
    o.answer();
    assert!(o.a.posting.is_none(), "posted with the middle never shown");

    // Reading on, by key and by wheel, does not take the question down.
    press(&mut o.a, "g");
    for _ in 0..60 {
        press(&mut o.a, "j");
        render(&mut o.a, 120, 20);
        if o.a.armed.as_ref().unwrap().unread() == 0 {
            break;
        }
    }
    mouse(
        &mut o.a,
        ratatui::crossterm::event::MouseEventKind::ScrollUp,
        60,
        10,
    );
    assert!(o.a.armed.is_some(), "reading withdrew the question");
    let screen = render(&mut o.a, 120, 20);
    assert_eq!(o.a.armed.as_ref().unwrap().unread(), 0);
    assert!(screen.contains("this is all of it"), "{screen}");
    assert!(
        footer(&frame(&mut o.a, 120, 20)).contains("y to post"),
        "the footer still withholds y"
    );

    // Laid out afresh for another width, they are other rows, and what was
    // read of the old ones does not count for them.
    render(&mut o.a, 60, 20);
    assert!(o.a.armed.as_ref().unwrap().unread() > 0);
    o.answer();
    assert!(o.a.posting.is_none(), "posted from a layout never read");

    press(&mut o.a, "g");
    for _ in 0..200 {
        render(&mut o.a, 60, 20);
        if o.a.armed.as_ref().unwrap().unread() == 0 {
            break;
        }
        press(&mut o.a, "j");
    }
    o.answer();
    assert!(o.a.posting.is_some());
    o.settle();
    assert_eq!(o.posts(), 1);
}

/// A window too small to show what would be posted without cutting it is a
/// window nothing is posted from. It says so, and says what to do.
#[test]
fn nothing_is_posted_from_a_window_too_small_to_show_it() {
    for (w, h) in [(34u16, 20u16), (120, 7)] {
        let mut o = Outbox::new();
        select_draft(&mut o.a, 4840);
        o.wait(1_000);
        press(&mut o.a, "a");
        let screen = render(&mut o.a, w, h);
        assert!(screen.contains("enlarge the window"), "{w}x{h}:\n{screen}");
        assert!(
            !screen.contains("this is what will be posted"),
            "{w}x{h}: it drew what it could not show whole:\n{screen}"
        );
        o.answer();
        render(&mut o.a, w, h);
        assert!(o.a.posting.is_none(), "{w}x{h}: posted unseen");
        assert!(o.a.armed.is_some());

        // Enlarged, it is shown, read, and can be answered.
        let screen = render(&mut o.a, 160, 40);
        assert!(screen.contains("this is what will be posted"), "{screen}");
        o.answer();
        assert!(o.a.posting.is_some(), "{w}x{h}");
        o.settle();
    }
    // The footer of a wide but short window has room to say why.
    let mut o = Outbox::new();
    select_draft(&mut o.a, 4840);
    press(&mut o.a, "a");
    let prompt = footer(&frame(&mut o.a, 160, 7));
    assert!(
        prompt.contains("the window is too small to show it: enlarge it to post"),
        "{prompt:?}"
    );
}

#[test]
fn confirmation_overlay_snapshots() {
    let mut o = Outbox::new();
    o.ask(4840);
    insta::assert_snapshot!("confirm_overlay_120x30", render(&mut o.a, 120, 30));

    let mut o = Outbox::new();
    long_draft(&mut o);
    select_draft(&mut o.a, 4840);
    press(&mut o.a, "a");
    insta::assert_snapshot!("confirm_overlay_long_120x20", render(&mut o.a, 120, 20));
    press(&mut o.a, "G");
    insta::assert_snapshot!("confirm_overlay_long_end_120x20", render(&mut o.a, 120, 20));
}
// --------------------------------------------------- withdrawing a question

/// The question answers to `a`, and reads with the movement keys. Anything
/// else takes it down — and does nothing more: a key pressed to get out of a
/// question about posting must not also quit, skip, or edit.
#[test]
fn any_other_key_withdraws_the_question_and_does_nothing_else() {
    use ratatui::crossterm::event::{MouseButton, MouseEventKind};
    let mut o = Outbox::new();
    let before = o.file(4840);

    for other in [
        "\x1b", "a", "?", "s", "e", "x", "q", "o", "c", "/", "\n", "r", "d", "1", "\t",
    ] {
        o.ask(4840);
        assert!(o.a.armed.is_some(), "before {other:?}");
        press(&mut o.a, other);
        assert!(o.a.armed.is_none(), "{other:?} left the question up");
        assert!(
            !o.a.quit && o.a.mode == Mode::Browse && o.a.view == View::Drafts,
            "{other:?} acted on the dashboard behind"
        );
        assert!(o.a.edit_request.is_none() && o.a.sort == Sort::Recent);
        assert!(!o.a.in_flight.prs, "{other:?} started a refresh");
        assert_eq!(o.file(4840), before, "{other:?} changed the draft");
    }

    o.ask(4840);
    mouse(&mut o.a, MouseEventKind::Down(MouseButton::Left), 4, 4);
    assert!(o.a.armed.is_none(), "a click left the question up");
    // Only the pointer passing over is not an answer of any kind.
    o.ask(4840);
    mouse(&mut o.a, MouseEventKind::Moved, 4, 4);
    assert!(
        o.a.armed.is_some(),
        "moving the pointer withdrew the question"
    );

    assert!(o.calls().is_empty(), "{:?}", o.calls());
}

/// Esc answers the prompt and nothing more: it must not also clear a filter
/// the user had narrowed the list with.
#[test]
fn esc_cancels_the_prompt_without_clearing_the_filter() {
    let mut o = Outbox::new();
    press(&mut o.a, "/palette\na\x1b");
    assert!(o.a.armed.is_none());
    assert_eq!(o.a.filter, "palette");
}

/// A refresh that leaves the draft as it was leaves the question standing;
/// one that changes it withdraws it, so `a` never confirms something the
/// overlay did not show.
#[test]
fn a_refresh_that_changes_the_row_withdraws_the_prompt() {
    let mut o = Outbox::new();

    o.ask(4840);
    o.a.reload_drafts();
    o.a.rebuild();
    assert!(o.a.armed.is_some(), "an unchanged draft lost its question");

    // The file is rewritten underneath.
    let text = draft_json(4840, |v| v["body"] = "Something else entirely.".into());
    std::fs::write(o.path(4840), text).unwrap();
    o.a.reload_drafts();
    assert!(o.a.armed.is_none(), "a rewritten draft kept its question");

    // The pull request moves on.
    o.ask(4840);
    assert!(o.a.armed.is_some());
    let moved = o.a.prs.iter_mut().find(|p| p.number == 4840).unwrap();
    moved.head_oid = MOVED.into();
    o.a.rebuild();
    assert!(o.a.armed.is_none(), "a stale draft kept its question");

    // The draft goes away, and the cursor lands on its neighbor.
    o.ask(4846);
    std::fs::remove_file(o.path(4846)).unwrap();
    o.a.reload_drafts();
    assert!(o.a.armed.is_none(), "the question outlived its draft");

    assert!(o.calls().is_empty() && o.a.posting.is_none());
}

/// A draft arriving above the one being asked about slides every row down
/// by one. The question is about a file, not a row: it stays on its draft,
/// and what is confirmed is that draft.
#[test]
fn a_draft_arriving_above_does_not_retarget_the_question() {
    let mut o = Outbox::new();
    o.ask(4840);
    // Sorts first: an open pull request that moved a moment ago.
    let mut fresh = o.a.prs[0].clone();
    fresh.number = 4900;
    fresh.updated_at = now_secs();
    o.a.prs.push(fresh);
    std::fs::write(o.path(4900), draft_json(4900, |_| {})).unwrap();
    o.a.reload_drafts();

    assert_eq!(draft_numbers(&o.a)[0], 4900);
    assert_eq!(cursor(&o.a), Some(4840), "the cursor slid to another draft");
    assert_eq!(o.a.armed.as_ref().map(|q| q.number), Some(4840));

    render(&mut o.a, 160, 40);
    o.answer();
    o.settle();
    assert_eq!(o.calls()[0], "pull 4840");
    assert_eq!(o.json(4840)["status"], "posted");
    assert_eq!(o.json(4900)["status"], "drafted");
}

/// Between the question and the answer the file can change without a
/// refresh having noticed. What would go out is then not what was shown.
#[test]
fn a_draft_changed_on_disk_after_the_prompt_is_not_sent() {
    let mut o = Outbox::new();
    o.ask(4840);
    // The same draft, saying something else: only its body differs.
    let mut edited = o.json(4840);
    edited["body"] = "On reflection, this needs more work.".into();
    let text = serde_json::to_string_pretty(&edited).unwrap();
    assert_ne!(text, o.file(4840));
    std::fs::write(o.path(4840), &text).unwrap();

    o.answer();
    assert!(o.a.posting.is_none() && o.calls().is_empty());
    assert!(o.note().contains("#4840 changed on disk"), "{}", o.note());
    assert_eq!(o.file(4840), text, "and the file is left alone");
}

// ------------------------------------------------- what GitHub says, on `a`

/// The drafts that never arm: `a`, twice, does not so much as ask GitHub, and
/// says why. Stale is the one that matters most — a review is only ever
/// offered for the exact commit it was written against.
#[test]
fn a_draft_that_must_not_be_posted_only_explains_itself() {
    let mut o = Outbox::new();
    for (number, why) in [
        (4700, "#4700 is stale"),
        (4980, "#4980 is invalid"),
        (4701, "#4701 is skipped"),
        (4801, "#4801 is already posted"),
    ] {
        let before = o.file(number);
        o.ask(number);
        assert!(o.a.armed.is_none(), "#{number} put up a question");
        assert!(o.note().starts_with(why), "#{number}: {}", o.note());
        o.answer();
        assert!(o.a.armed.is_none() && o.a.posting.is_none(), "#{number}");
        assert_eq!(o.file(number), before, "#{number} was rewritten");
    }
    assert!(o.calls().is_empty(), "{:?}", o.calls());
}

/// A pull request that moved on after its draft was loaded is stale from the
/// next fetch, without the file changing at all.
#[test]
fn a_ready_draft_goes_stale_when_the_pull_request_moves() {
    let mut o = Outbox::new();
    let moved = o.a.prs.iter_mut().find(|p| p.number == 4846).unwrap();
    moved.head_oid = MOVED.into();
    o.a.rebuild();
    assert!(draft_row(&mut o.a, 4846).contains(" stale "));
    o.ask(4846);
    o.answer();
    assert!(o.calls().is_empty() && o.a.posting.is_none());
}

/// When GitHub declines the review — an approval it will not take, a comment
/// on a line that is not in the diff — nothing went out. The screen says so in
/// GitHub's words, the file is byte for byte what it was, and the row is still
/// a draft: fix it, and it can be sent.
#[test]
fn a_refusal_from_github_is_shown_and_the_draft_stays_postable() {
    let mut o = Outbox::new();
    o.gh.post_answers(Err(PostFailure::Declined(
        "Validation Failed: line must be part of the diff (HTTP 422)".into(),
    )));
    let before = o.file(4840);
    o.confirm(4840);

    assert_eq!(o.posts(), 1, "it was attempted once");
    assert_eq!(o.file(4840), before);
    assert!(draft_row(&mut o.a, 4840).contains(" comment +2 "));
    let screen = render(&mut o.a, 160, 24);
    assert!(
        screen.contains(
            "✗ posting to #4840 failed: Validation Failed: line must be part of the diff (HTTP 422)"
        ),
        "{screen}"
    );

    // The offending comment is edited out, and the draft goes through. The
    // old failure is gone as soon as the new attempt is under way: it is not
    // left up to be read as the answer to this one.
    let fixed = draft_json(4840, |v| v["comments"] = serde_json::json!([]));
    std::fs::write(o.path(4840), fixed).unwrap();
    o.a.reload_drafts();
    o.gh.post_answers(Ok(None));
    o.ask(4840);
    o.answer();
    assert!(
        o.a.post_error.is_none(),
        "the last attempt's failure is still up"
    );
    o.settle();
    assert_eq!(o.posts(), 2);
    assert_eq!(o.json(4840)["status"], "posted");
    assert!(o.a.post_error.is_none(), "the next attempt cleared it");
}

/// What went wrong with a post is not something to type past. It stays on
/// its own line through other keys and through filter mode, until `esc`
/// dismisses it or another attempt replaces it.
#[test]
fn a_post_failure_stays_until_esc_or_the_next_attempt() {
    let mut o = Outbox::new();
    o.gh.post_answers(Err(PostFailure::Declined("Not allowed (HTTP 403)".into())));
    o.confirm(4840);
    let shown = |a: &mut App| render(a, 160, 24).contains("✗ posting to #4840 failed: Not allowed");
    assert!(shown(&mut o.a));

    // Moving about, a sync coming good, opening and closing help.
    press(&mut o.a, "jkJK??");
    o.a.error = None;
    assert!(shown(&mut o.a), "a keypress swept it away");

    // Typing a filter: the footer is the filter's, and the failure is still
    // on screen above it.
    press(&mut o.a, "/pal");
    let screen = render(&mut o.a, 160, 24);
    assert!(
        screen.contains("posting to #4840 failed") && screen.contains("/pal"),
        "hidden while filtering:\n{screen}"
    );
    press(&mut o.a, "\n");
    assert!(shown(&mut o.a));

    // One thing per esc: the failure first, and the filter is still there.
    press(&mut o.a, "\x1b");
    assert!(o.a.post_error.is_none() && o.a.filter == "pal");
    assert!(!shown(&mut o.a));
    press(&mut o.a, "\x1b");
    assert!(o.a.filter.is_empty());
}

/// While an attempt is on its way, `a` — however often, on whatever row —
/// starts nothing more, and the draft it is about is left alone: `e` and `x`
/// wait until the answer is in.
#[test]
fn a_post_in_flight_is_not_sent_twice_and_its_draft_is_left_alone() {
    let mut o = Outbox::new();
    // GitHub holds the write until the test lets it go.
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    o.gh.on_post(move |_| {
        let _ = gate.lock().unwrap().recv();
    });

    o.ask(4840);
    o.answer();
    assert!(o.a.posting.is_some());
    // Only its own row says so.
    assert!(draft_row(&mut o.a, 4840).contains(" posting… "));
    assert!(draft_row(&mut o.a, 4846).contains(" approve "));
    select_draft(&mut o.a, 4840);
    for _ in 0..4 {
        o.wait(1_000);
        press(&mut o.a, "a");
        assert!(o.a.armed.is_none(), "a second question went up mid-flight");
    }
    assert_eq!(o.note(), "a review is already being posted");

    // Nor can the draft be set aside or edited from under its own review.
    press(&mut o.a, "x");
    assert_eq!(o.json(4840)["status"], "drafted");
    assert_eq!(
        o.note(),
        "#4840 is being posted; x waits until that is done"
    );
    press(&mut o.a, "e");
    assert_eq!(o.a.edit_request, None, "the editor was opened on it");
    assert_eq!(
        o.note(),
        "#4840 is being posted; e waits until that is done"
    );

    // Another row is held back too; its own `e` is not.
    o.ask(4846);
    assert!(o.a.armed.is_none());
    press(&mut o.a, "e");
    assert_eq!(o.a.edit_request.take(), Some(o.path(4846)));

    release.send(()).unwrap();
    o.settle();
    assert_eq!(
        o.calls().iter().filter(|c| c.starts_with("pull ")).count(),
        1,
        "a second attempt was started: {:?}",
        o.calls()
    );
    assert_eq!(o.posts(), 1);

    // And once it has landed, the row is posted, not postable.
    o.ask(4840);
    assert!(o.a.armed.is_none());
    assert_eq!(o.posts(), 1);
}

/// The answer to a post is about the draft that was sent. If the file has
/// come to say something else while the answer was on its way, marking *that*
/// posted would record a review that never went out — so what was sent is
/// written back, and he is told.
#[test]
fn a_file_changed_while_posting_keeps_what_was_sent() {
    let mut o = Outbox::new();
    let sent = o.json(4840);
    let path = o.path(4840);
    // Regenerated under the review, with another verdict and other words.
    let other = draft_json(4840, |v| {
        v["verdict"] = "approve".into();
        v["body"] = "A different review altogether.".into();
        v["comments"] = serde_json::json!([]);
    });
    o.gh.on_post(move |_| std::fs::write(&path, &other).unwrap());
    o.confirm(4840);

    let mut want = sent;
    want["status"] = "posted".into();
    want["posted_at"] = POSTED_NOW.into();
    assert_eq!(o.json(4840), want, "the file is not what was posted");
    assert_eq!(
        o.note(),
        "posted to #4840; the file changed while posting, and what was sent was kept"
    );
}

/// A review that went out but could not be written down is still out. The
/// next attempt finds it on GitHub, marks the draft posted, and sends nothing.
#[test]
fn a_review_that_was_sent_but_not_recorded_is_found_not_sent_again() {
    let mut o = Outbox::new();
    let text = o.file(4840);
    let folder = o.path(4840).parent().unwrap().to_path_buf();
    let gone = folder.clone();
    // The whole drafts folder vanishes while the review is on its way, so
    // there is nowhere to write the record; GitHub takes the review.
    o.gh.on_post(move |gh| {
        std::fs::remove_dir_all(&gone).unwrap();
        gh.has_4840s_review();
    });
    o.confirm(4840);
    assert!(
        o.failure()
            .starts_with("the review was posted to #4840, but its draft file could not be updated"),
        "{}",
        o.failure()
    );

    // Whatever wrote the draft writes it again, still `drafted`.
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(o.path(4840), text).unwrap();
    o.a.reload_drafts();
    o.confirm(4840);
    assert_eq!(o.posts(), 1, "{:?}", o.calls());
    assert_eq!(o.json(4840)["status"], "posted");
}

/// A write that got no answer is not a failure: the review may be there. The
/// screen says that, the row stops reading as a plain draft, and trying again
/// asks GitHub first — which finds the review, stamped when the app sent it,
/// and posts nothing twice.
#[test]
fn a_post_that_got_no_answer_is_unknown_and_is_found_not_sent_again() {
    let mut o = Outbox::new();
    o.gh.post_answers(Err(PostFailure::NoAnswer(
        "gh api: timed out after 45s".into(),
    )));
    o.gh.on_post(FakeGitHub::has_4840s_review);
    let before = o.file(4840);
    o.confirm(4840);

    assert_eq!(
        o.failure(),
        "#4840: the review was sent, and whether it was posted is not known (gh api: timed out after 45s). a asks again, and GitHub is checked before anything is sent"
    );
    assert!(
        !o.failure().contains("failed"),
        "an unknown outcome was reported as a failure"
    );
    assert_eq!(o.file(4840), before, "nothing is recorded on a guess");
    let row = draft_row(&mut o.a, 4840);
    assert!(
        row.contains(" maybe sent ") && !row.contains("comment +2"),
        "{row:?}"
    );

    o.confirm(4840);
    assert_eq!(o.posts(), 1, "posted twice: {:?}", o.calls());
    assert_eq!(
        o.calls()[3..],
        ["pull 4840", "reviews 4840", "comments 4840"],
        "the second attempt read, and did not write"
    );
    let after = o.json(4840);
    assert_eq!(after["status"], "posted");
    assert_eq!(
        after["posted_at"], POSTED_NOW,
        "posted when GitHub says it was"
    );
    assert!(o.note().contains("already has this review"), "{}", o.note());
    assert!(draft_row(&mut o.a, 4840).contains(" ✓ posted "));
}

/// And when the unanswered write did not land, trying again finds nothing
/// there, sends it, and the row is an ordinary one again.
#[test]
fn a_post_that_got_no_answer_and_did_not_land_is_sent_on_the_next_try() {
    let mut o = Outbox::new();
    o.gh.post_answers(Err(PostFailure::NoAnswer("HTTP 502".into())));
    o.confirm(4840);
    assert!(draft_row(&mut o.a, 4840).contains(" maybe sent "));

    // The label is about a draft that could still be sent. Set aside by
    // hand in the meantime, the row says that instead.
    let drafted = o.file(4840);
    let mut aside = o.json(4840);
    aside["status"] = "skipped".into();
    std::fs::write(o.path(4840), serde_json::to_string_pretty(&aside).unwrap()).unwrap();
    o.a.reload_drafts();
    assert!(draft_row(&mut o.a, 4840).contains(" skipped "));
    std::fs::write(o.path(4840), drafted).unwrap();
    o.a.reload_drafts();

    // A try that cannot reach GitHub settles nothing.
    o.gh.reviews_are(Err("HTTP 502".into()));
    o.confirm(4840);
    assert!(draft_row(&mut o.a, 4840).contains(" maybe sent "));

    // A try that reads the reviews and does not find it there settles it,
    // whatever else it is refused for: the row is a plain draft again.
    o.gh.reviews_are(Ok(vec![review_by_viewer("CHANGES_REQUESTED", "Not yet.")]));
    o.confirm(4840);
    assert_eq!(o.posts(), 1);
    let row = draft_row(&mut o.a, 4840);
    assert!(
        row.contains(" comment +2 ") && !row.contains("maybe sent"),
        "{row:?}"
    );

    o.gh.reviews_are(Ok(Vec::new()));
    o.gh.post_answers(Ok(None));
    o.confirm(4840);
    assert_eq!(o.posts(), 2);
    assert!(draft_row(&mut o.a, 4840).contains(" ✓ posted "));
}

/// The list is as old as the last refresh. A pull request that has moved on
/// since is caught by the read on the confirming keypress: nothing is sent,
/// and the row is stale from then on — also when the list had never seen the
/// pull request at all.
#[test]
fn a_pull_request_that_moved_since_the_list_was_fetched_is_refused_and_goes_stale() {
    for number in [4840, 4990] {
        let mut o = Outbox::new();
        o.gh.pull_is(number, Ok(live(true, false, MOVED)));
        let before = o.file(number);
        o.confirm(number);

        assert_eq!(
            o.calls(),
            [format!("pull {number}")],
            "nothing after the head"
        );
        assert_eq!(o.file(number), before);
        assert_eq!(
            o.failure(),
            format!(
                "nothing posted: #{number} has moved on to 9f8e7d6 since this draft was written"
            )
        );
        assert!(draft_row(&mut o.a, number).contains(" stale "));
        select_draft(&mut o.a, number);
        let detail = render(&mut o.a, 160, 30);
        assert!(
            detail.contains("the pull request is now at 9f8e7d6"),
            "{detail}"
        );
        // Stale never arms: there is no second try against the same head.
        o.ask(number);
        assert!(o.a.armed.is_none() && o.a.posting.is_none());
        assert_eq!(o.calls().len(), 1);
    }
}

/// What a confirming read learned holds until a fetch knows better. A fetch
/// that covers the pull request is newer than the read, and replaces it.
#[test]
fn a_later_fetch_replaces_what_the_confirming_read_saw() {
    let mut o = Outbox::new();
    o.gh.pull_is(4840, Ok(live(true, false, MOVED)));
    o.confirm(4840);
    assert!(draft_row(&mut o.a, 4840).contains(" stale "));

    // The force-push was undone: the next fetch has the old head again.
    o.a.on_msg(crate::app::Msg::Prs(Ok(crate::github::Fetched {
        budget: None,
        viewer: "octocat".into(),
        default_branch: "main".into(),
        prs: o.a.prs.clone(),
        merged: o.a.merged.clone(),
    })));
    assert!(draft_row(&mut o.a, 4840).contains(" comment +2 "));
}

/// A pull request closed without merging takes no review. A merged one still
/// at the draft's commit does.
#[test]
fn a_closed_pull_request_is_refused_and_a_merged_one_is_not() {
    let mut o = Outbox::new();
    o.gh.pull_is(4840, Ok(live(false, false, crate::drafts::tests::HEAD)));
    let before = o.file(4840);
    o.confirm(4840);
    assert_eq!(o.calls(), ["pull 4840"]);
    assert_eq!(
        o.failure(),
        "nothing posted: #4840 was closed without being merged"
    );
    assert_eq!(o.file(4840), before);
    assert!(draft_row(&mut o.a, 4840).contains(" comment +2 "));

    o.gh.pull_is(4840, Ok(live(false, true, crate::drafts::tests::HEAD)));
    o.confirm(4840);
    assert_eq!(o.posts(), 1);
    assert_eq!(o.json(4840)["status"], "posted");
}

/// The same review already on the pull request — its inline comments read
/// and found the same — means the draft was sent: it is marked posted, with
/// the time GitHub gives, and nothing goes out.
#[test]
fn a_review_already_on_github_marks_the_draft_posted_and_sends_nothing() {
    let mut o = Outbox::new();
    o.gh.has_4840s_review();
    let before = o.json(4840);
    o.confirm(4840);

    assert_eq!(o.calls(), ["pull 4840", "reviews 4840", "comments 4840"]);
    let mut want = before;
    want["status"] = "posted".into();
    want["posted_at"] = POSTED_NOW.into();
    assert_eq!(o.json(4840), want);
    assert!(o.a.post_error.is_none());
    assert_eq!(
        o.note(),
        "#4840 already has this review from you; marked posted, nothing sent"
    );
}

/// A review with the same verdict, commit and words but other inline
/// comments is another review. The draft is not marked posted on its
/// account: its own comments were never sent.
#[test]
fn a_review_that_only_looks_the_same_does_not_mark_the_draft_posted() {
    let mut o = Outbox::new();
    o.gh.reviews_are(Ok(vec![review_by_viewer("COMMENTED", BODY_4840)]));
    o.gh.comments_are(Ok(vec![comments_4840().remove(0)]));
    let before = o.file(4840);
    o.confirm(4840);

    assert_eq!(o.posts(), 0, "{:?}", o.calls());
    assert_eq!(o.file(4840), before, "a draft never sent was marked posted");
    assert!(
        o.failure().contains("you have reviewed #4840 since"),
        "{}",
        o.failure()
    );
}

/// A different review from the viewer since the draft was written means they
/// have already said something else. The draft is not sent on top of it, and
/// there is no key that sends it anyway.
#[test]
fn a_different_review_since_the_draft_was_written_blocks_it() {
    let mut o = Outbox::new();
    o.gh.reviews_are(Ok(vec![review_by_viewer(
        "CHANGES_REQUESTED",
        "Not yet: the cap is wrong.",
    )]));
    let before = o.file(4840);
    for _ in 0..2 {
        o.confirm(4840);
        assert_eq!(o.posts(), 0, "{:?}", o.calls());
        assert_eq!(o.file(4840), before);
        assert_eq!(
            o.failure(),
            "nothing posted: you have reviewed #4840 since this draft was written; x skips it, or review in the browser"
        );
        assert!(draft_row(&mut o.a, 4840).contains(" comment +2 "));
    }
    assert_eq!(
        o.calls(),
        ["pull 4840", "reviews 4840", "pull 4840", "reviews 4840"]
    );
}

/// A review started in the browser and left unsubmitted is the one review
/// GitHub lets him have open. It is said plainly, before anything is sent,
/// rather than left for GitHub to refuse.
#[test]
fn a_pending_review_of_his_own_is_refused_before_anything_is_sent() {
    let mut o = Outbox::new();
    o.gh.reviews_are(Ok(vec![Review {
        state: "PENDING".into(),
        submitted_at: None,
        body: String::new(),
        ..review_by_viewer("PENDING", "")
    }]));
    let before = o.file(4840);
    o.confirm(4840);
    assert_eq!(o.calls(), ["pull 4840", "reviews 4840"]);
    assert_eq!(
        o.failure(),
        "nothing posted: you have a review of #4840 started and not submitted; submit or discard it in the browser first"
    );
    assert_eq!(o.file(4840), before);
}

/// Another rigor window is posting this very draft. It is left to it.
#[test]
fn a_draft_another_window_is_posting_is_not_posted_from_this_one() {
    let mut o = Outbox::new();
    let lock = o.path(4840).with_file_name(".4840.json.lock");
    std::fs::write(&lock, "4242\n").unwrap();
    o.confirm(4840);
    assert!(o.calls().is_empty(), "{:?}", o.calls());
    assert_eq!(
        o.failure(),
        "nothing posted: another rigor window is posting #4840"
    );
    // The lock is not a draft, and is not this window's to remove.
    assert!(lock.exists());
    assert_eq!(o.a.drafts.len(), 7);
}

/// If GitHub cannot be asked, nothing is sent on the list's word alone.
#[test]
fn a_failed_read_sends_nothing() {
    let timed_out = "gh api: timed out after 45s".to_string();

    let mut o = Outbox::new();
    o.gh.pull_is(4840, Err(timed_out.clone()));
    let before = o.file(4840);
    o.confirm(4840);
    assert_eq!(o.calls(), ["pull 4840"]);
    assert_eq!(
        o.failure(),
        "nothing posted: could not check #4840 on GitHub: gh api: timed out after 45s"
    );
    assert_eq!(o.file(4840), before);

    let mut o = Outbox::new();
    o.gh.reviews_are(Err(timed_out.clone()));
    o.confirm(4840);
    assert_eq!(o.calls(), ["pull 4840", "reviews 4840"]);
    assert!(
        o.failure().contains("could not check #4840"),
        "{}",
        o.failure()
    );

    // The read that tells one review from another, too.
    let mut o = Outbox::new();
    o.gh.reviews_are(Ok(vec![review_by_viewer("COMMENTED", BODY_4840)]));
    o.gh.comments_are(Err(timed_out));
    o.confirm(4840);
    assert_eq!(o.posts(), 0, "{:?}", o.calls());
    assert!(o.failure().contains("could not check #4840"));
    assert_eq!(o.file(4840), before);
    assert!(draft_row(&mut o.a, 4840).contains(" comment +2 "));
}

/// Until a fetch has said who the viewer is, their reviews cannot be told
/// from anyone else's, so nothing is sent.
#[test]
fn nothing_is_posted_before_the_viewer_is_known() {
    let mut o = Outbox::new();
    o.a.viewer.clear();
    o.confirm(4840);
    assert!(o.calls().is_empty(), "{:?}", o.calls());
    assert!(o.failure().contains("your GitHub login is not known yet"));
}

/// A draft whose pull request the list never fetched can still be posted:
/// the prompt says the list cannot vouch for it, and the read on the
/// confirming keypress does the checking the list could not.
#[test]
fn an_unverified_draft_is_checked_on_github_and_then_posted() {
    let mut o = Outbox::new();
    assert!(draft_row(&mut o.a, 4990).contains(" unverified "));

    o.ask(4990);
    let prompt = footer(&frame(&mut o.a, 160, 40));
    assert!(
        prompt.contains(
            "post approve with 1 inline comment to #4990? not fetched here, so GitHub is asked for its head first. y to post, esc to cancel"
        ),
        "{prompt:?}"
    );
    assert!(o.calls().is_empty(), "one a reached GitHub");
    let overlay = render(&mut o.a, 160, 40);
    assert!(
        overlay.contains("#4990 — not among the fetched pull requests"),
        "{overlay}"
    );

    o.answer();
    o.settle();
    assert_eq!(o.calls()[..2], ["pull 4990", "reviews 4990"]);
    assert_eq!(o.posts(), 1);
    assert_eq!(o.json(4990)["status"], "posted");
    assert!(draft_row(&mut o.a, 4990).contains(" ✓ posted "));
}

/// A question put up for an unverified draft does not carry over once a
/// fetch has seen the pull request: the question has changed, so it is asked
/// again.
#[test]
fn a_fetch_that_verifies_the_draft_withdraws_the_unverified_prompt() {
    let mut o = Outbox::new();
    o.ask(4990);
    assert!(o.a.armed.is_some());
    let mut seen = o.a.prs[0].clone();
    seen.number = 4990;
    o.a.prs.push(seen);
    o.a.rebuild();
    assert!(o.a.armed.is_none());
    assert!(o.calls().is_empty());
}

/// `a` asks only from the Drafts view, and neither it nor `y` does anything
/// as a chord.
#[test]
fn a_and_y_do_nothing_outside_the_drafts_view_or_as_a_chord() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut o = Outbox::new();
    o.a.set_view(View::All);
    o.wait(1_000);
    press(&mut o.a, "a");
    assert!(o.a.armed.is_none() && o.a.posting.is_none());

    o.a.set_view(View::Drafts);
    select_draft(&mut o.a, 4840);
    for modifier in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
        o.wait(1_000);
        crate::event::key(&mut o.a, KeyEvent::new(KeyCode::Char('a'), modifier));
        assert!(
            o.a.armed.is_none() && o.a.posting.is_none(),
            "{modifier:?}-a asked"
        );
        // Nor does a chord answer a question that is up: it takes it down.
        o.ask(4840);
        o.wait(1_000);
        crate::event::key(&mut o.a, KeyEvent::new(KeyCode::Char('y'), modifier));
        assert!(
            o.a.armed.is_none() && o.a.posting.is_none(),
            "{modifier:?}-y answered"
        );
    }
    assert!(o.calls().is_empty());
}

/// `x` sets a draft aside and brings it back, touching only its status. It
/// never rewrites a draft it cannot read, or one already posted.
#[test]
fn x_skips_a_draft_and_brings_it_back() {
    let mut o = Outbox::new();
    select_draft(&mut o.a, 4846);
    let before = o.json(4846);

    press(&mut o.a, "x");
    let mut want = before.clone();
    want["status"] = "skipped".into();
    assert_eq!(o.json(4846), want);
    assert!(draft_row(&mut o.a, 4846).contains(" skipped "));
    // A skipped draft is not postable until it is brought back.
    o.ask(4846);
    assert!(o.a.armed.is_none());

    press(&mut o.a, "x");
    assert_eq!(o.json(4846), before, "back exactly as it was");
    assert!(draft_row(&mut o.a, 4846).contains(" approve "));

    // Stale and unverified drafts can be set aside too: that is what one does
    // with a draft that will never be posted.
    select_draft(&mut o.a, 4700);
    press(&mut o.a, "x");
    assert_eq!(o.json(4700)["status"], "skipped");

    for (number, why) in [(4801, "is already posted"), (4980, "is invalid")] {
        let before = o.file(number);
        select_draft(&mut o.a, number);
        press(&mut o.a, "x");
        assert_eq!(o.file(number), before, "#{number} was rewritten");
        assert!(o.note().contains(why), "#{number}: {}", o.note());
    }
    assert!(o.calls().is_empty());
}

/// `e` hands the selected draft's file to the event loop, which owns the
/// terminal and so runs the editor. Any draft can be edited, a broken one most
/// of all.
#[test]
fn e_asks_for_the_selected_drafts_file() {
    let mut o = Outbox::new();
    select_draft(&mut o.a, 4980);
    press(&mut o.a, "e");
    assert_eq!(o.a.edit_request, Some(o.path(4980)));

    o.a.edit_request = None;
    o.a.set_view(View::All);
    press(&mut o.a, "e");
    assert_eq!(o.a.edit_request, None, "only in the Drafts view");
}

/// The footer offers what would do something on this row, and no more.
#[test]
fn the_footer_offers_only_what_applies_to_the_draft() {
    let mut o = Outbox::new();
    let mut keys = |number: u64| {
        select_draft(&mut o.a, number);
        footer(&frame(&mut o.a, 160, 24))
    };
    let ready = keys(4846);
    assert!(
        ready.contains("a post") && ready.contains("e edit") && ready.contains("x skip"),
        "{ready:?}"
    );
    let stale = keys(4700);
    assert!(
        !stale.contains("a post") && stale.contains("x skip"),
        "{stale:?}"
    );
    let unverified = keys(4990);
    assert!(
        unverified.contains("a post") && unverified.contains("x skip"),
        "{unverified:?}"
    );
    let skipped = keys(4701);
    assert!(
        !skipped.contains("a post") && skipped.contains("x unskip"),
        "{skipped:?}"
    );
    let invalid = keys(4980);
    assert!(
        !invalid.contains("a post") && invalid.contains("e edit") && !invalid.contains("x "),
        "{invalid:?}"
    );
    let posted = keys(4801);
    assert!(
        !posted.contains("a post") && !posted.contains("x "),
        "{posted:?}"
    );
}

/// A file in the drafts folder that is not named for a pull request has no
/// number. It is invalid, and says so — with nothing in flight, and with
/// some other draft in flight.
#[test]
fn a_file_with_no_number_is_never_shown_as_posting() {
    let mut o = Outbox::new();
    std::fs::write(
        o.path(4840).with_file_name("notes.json"),
        draft_json(1, |_| {}),
    )
    .unwrap();
    o.a.reload_drafts();
    let row = |a: &mut App| {
        render(a, 160, 24)
            .lines()
            .find(|l| l.starts_with(" notes.json "))
            .map(String::from)
            .expect("no row for the misnamed file")
    };
    let idle = row(&mut o.a);
    assert!(
        idle.contains(" invalid ") && !idle.contains("posting"),
        "{idle:?}"
    );

    let (release, gate) = std::sync::mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    o.gh.on_post(move |_| {
        let _ = gate.lock().unwrap().recv();
    });
    o.ask(4840);
    o.answer();
    let busy = row(&mut o.a);
    assert!(
        busy.contains(" invalid ") && !busy.contains("posting"),
        "{busy:?}"
    );
    release.send(()).unwrap();
    o.settle();
}

/// GitHub answered the post, and `gh` exited cleanly — but what came back is
/// not this review. That is what a request redirected and followed as a read
/// looks like. It is not recorded as posted on that evidence.
#[test]
fn an_answer_that_is_not_this_review_is_not_recorded_as_posted() {
    let mut o = Outbox::new();
    let before = o.file(4840);
    // Somebody else's review, of another kind, on another commit.
    o.gh.post_answers(Ok(Some(Review {
        id: 5,
        author: "hubot".into(),
        state: "APPROVED".into(),
        commit: MOVED.into(),
        body: String::new(),
        submitted_at: Some(POSTED_NOW.into()),
    })));
    o.confirm(4840);
    assert_eq!(o.posts(), 1);
    assert_eq!(
        o.file(4840),
        before,
        "marked posted on gh's exit status alone"
    );
    assert!(
        o.failure().contains(
            "whether it was posted is not known (GitHub answered, but not with this review)"
        ),
        "{}",
        o.failure()
    );
    assert!(draft_row(&mut o.a, 4840).contains(" maybe sent "));
}

/// The draft is read again when the answer comes, and read the way the list
/// reads it: only ever as a regular file. A link swapped in under the
/// question, even to a file that says the same, is not the draft that was
/// shown.
#[cfg(unix)]
#[test]
fn a_draft_swapped_for_a_link_after_the_question_is_not_posted() {
    let mut o = Outbox::new();
    o.ask(4840);
    let elsewhere = o.dir.path().join("elsewhere.json");
    std::fs::rename(o.path(4840), &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, o.path(4840)).unwrap();
    assert_eq!(o.file(4840), std::fs::read_to_string(&elsewhere).unwrap());

    o.answer();
    assert!(
        o.a.posting.is_none() && o.calls().is_empty(),
        "{:?}",
        o.calls()
    );
    assert!(o.note().contains("#4840 changed on disk"), "{}", o.note());
}

// ------------------------------------------------------------- on the way out

/// Rigor on its way out lets a post in flight finish, and records it, without
/// reading anything from a terminal that may be gone.
#[test]
fn leaving_waits_for_the_post_in_flight_and_records_it() {
    let mut o = Outbox::new();
    assert_eq!(
        o.a.finish_post(&o.replies, std::time::Duration::from_secs(5)),
        None,
        "nothing in flight, nothing to wait for"
    );

    let (release, gate) = std::sync::mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    o.gh.on_post(move |_| {
        let _ = gate.lock().unwrap().recv();
    });
    o.ask(4840);
    o.answer();
    assert!(o.a.posting.is_some());

    // GitHub answers a moment after rigor is asked to go.
    let later = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        release.send(()).unwrap();
    });
    let said =
        o.a.finish_post(&o.replies, std::time::Duration::from_secs(10));
    later.join().unwrap();
    assert_eq!(said.as_deref(), Some("posted to #4840"));
    assert_eq!(o.json(4840)["status"], "posted");
    assert!(o.a.posting.is_none() && o.a.quitting);
}

/// The wait has one limit, for the whole of what is left. Past it rigor goes
/// anyway — and says the outcome is not known, rather than that it failed,
/// and leaves the file saying what is true of it.
#[test]
fn leaving_gives_up_after_its_limit_and_says_the_outcome_is_unknown() {
    let mut o = Outbox::new();
    let before = o.file(4840);
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    o.gh.on_post(move |_| {
        let _ = gate.lock().unwrap().recv();
    });
    o.ask(4840);
    o.answer();

    let started = std::time::Instant::now();
    let said =
        o.a.finish_post(&o.replies, std::time::Duration::from_millis(200))
            .expect("something to say");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "the wait was not bounded: {:?}",
        started.elapsed()
    );
    assert!(
        said.contains("the post to #4840 did not finish")
            && said.contains("may or may not be posted"),
        "{said}"
    );
    assert!(!said.contains("failed"), "{said}");
    assert_eq!(o.file(4840), before, "recorded on a guess");
    assert!(o.a.posting.is_none());
    release.send(()).unwrap();
}

/// However the event loop ended, the way out is the same. An error from a
/// terminal that went away is not a reason to cut a review off half-sent: the
/// post is waited for and recorded, nothing is drawn to the terminal that
/// failed, and the error is handed on as it was.
#[test]
fn leaving_on_an_error_still_waits_for_the_post_and_records_it() {
    for ended in [Ok(()), Err("the terminal went away")] {
        let mut o = Outbox::new();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        o.gh.on_post(move |_| {
            let _ = gate.lock().unwrap().recv();
        });
        o.ask(4840);
        o.answer();
        let later = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            release.send(()).unwrap();
        });

        let mut drawn = false;
        let (handed_on, said) = o.a.leave(
            ended,
            &o.replies,
            std::time::Duration::from_secs(10),
            |_| drawn = true,
        );
        later.join().unwrap();
        assert_eq!(handed_on, ended);
        assert_eq!(said.as_deref(), Some("posted to #4840"), "{ended:?}");
        assert_eq!(o.json(4840)["status"], "posted", "{ended:?}");
        assert_eq!(
            drawn,
            ended.is_ok(),
            "a terminal that failed is not drawn to"
        );
    }

    // With nothing in flight there is nothing to wait for or to say.
    let mut o = Outbox::new();
    let (handed_on, said) = o.a.leave(
        Err("x"),
        &o.replies,
        std::time::Duration::from_secs(10),
        |_| {},
    );
    assert_eq!((handed_on, said), (Err("x"), None));
    assert!(!o.a.quitting);
}
