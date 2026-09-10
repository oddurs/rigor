//! Layout checks. These render real frames into an in-memory backend, so
//! column alignment and width fallbacks are verified without a terminal.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::draw;
use crate::app::{App, Sort, WtState};
use crate::config::{LayoutMode, Settings, View};
use crate::model::{
    Check, CheckState, Mergeable, MergedPr, Pr, RepoInfo, ReviewDecision, Worktree, WorktreeStatus,
};
use crate::theme::Theme;
use crate::util::now_secs;

/// The instant every fixture is built and rendered at. Arbitrary but fixed.
const NOW: i64 = 1_800_000_000;

fn sample_app() -> App {
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
    let (mut a, _rx) = App::new(repo, settings, Theme::default());
    a.home = Some("/home/dev".into());
    let now = now_secs();

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
            "Move the palette onto colour tokens",
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
                merged_at: now - 86400 * 4,
            },
            MergedPr {
                number: 4701,
                title: "Fix the row hover state".into(),
                url: "https://github.com/acme/widget/pull/4701".into(),
                head_ref: "row-hover".into(),
                merged_at: now - 86400 * 5,
            },
        ],
    })));

    a.on_msg(crate::app::Msg::Worktrees(Ok(vec![
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
    ])));
    a
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
            a.show_help = help;
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
    a.show_help = false;
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
            _ => unreachable!(),
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
        _ => unreachable!(),
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
/// subnav, and it survives NO_COLOR because it is a glyph, not a colour.
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
/// stuck. They take the success and failure colours when non-zero.
#[test]
fn ready_and_blocked_counts_carry_meaning_in_colour() {
    let mut a = sample_app();
    a.set_view(View::All);
    let buf = frame(&mut a, 160, 24);
    let subnav = row_text(&buf, 1);
    let digit_after = |title: &str| {
        let col = subnav.find(title).unwrap() + title.len() + 1;
        buf[(col as u16, 1)].fg
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
fn the_selected_row_is_a_full_width_band_when_the_terminal_reports_its_colours() {
    let mut a = sample_app();
    a.theme = Theme::resolve(
        &crate::theme::ThemeConfig::default(),
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
    let dot_colour = |a: &mut App| {
        let buf = frame(a, 160, 24);
        let row = row_text(&buf, 0);
        let col = row.chars().position(|c| c == '●').expect("no status dot") as u16;
        buf[(col, 0)].fg
    };

    let mut a = sample_app();
    a.last_refresh = now_secs() - 5;
    assert_eq!(dot_colour(&mut a), a.theme.success);

    a.last_refresh = now_secs() - (a.settings.refresh_secs as i64 * 3);
    assert_eq!(dot_colour(&mut a), a.theme.pending);

    a.error = Some("gh api graphql failed".into());
    assert_eq!(dot_colour(&mut a), a.theme.failure);
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
    a.filter_mode = true;
    a.filter = "retry".into();
    a.rebuild();
    let f = footer(&frame(&mut a, 160, 24));
    assert!(f.contains("/retry") && f.contains("1 of 3"), "{f:?}");
}

/// The help modal sits on a dimmed background, so it reads as a layer.
#[test]
fn the_help_modal_dims_what_is_behind_it() {
    let mut a = sample_app();
    a.show_help = true;
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

fn press(a: &mut App, input: &mut crate::event::Input, keys: &str) {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for ch in keys.chars() {
        let code = match ch {
            '\n' => KeyCode::Enter,
            '\x1b' => KeyCode::Esc,
            '\x08' => KeyCode::Backspace,
            '\t' => KeyCode::Tab,
            c => KeyCode::Char(c),
        };
        input.key(a, KeyEvent::new(code, KeyModifiers::NONE));
    }
}

/// The keyboard is the primary interface; its contract is pinned here.
#[test]
fn keys_move_the_selection_and_switch_views_by_position() {
    let mut a = sample_app();
    let mut i = crate::event::Input::default();
    a.set_view(View::All);
    press(&mut a, &mut i, "j");
    assert_eq!(a.selected, 1);
    press(&mut a, &mut i, "G");
    assert_eq!(a.selected, a.rows.len() - 1);
    press(&mut a, &mut i, "k");
    assert_eq!(a.selected, a.rows.len() - 2);
    press(&mut a, &mut i, "g");
    assert_eq!(a.selected, 0);

    // Number keys index the tab bar, whatever it is configured to hold.
    press(&mut a, &mut i, "6");
    assert_eq!(a.view, View::Worktrees);
    press(&mut a, &mut i, "1");
    assert_eq!(a.view, View::Ready);
    press(&mut a, &mut i, "\t");
    assert_eq!(a.view, View::Mine);
    press(&mut a, &mut i, "9"); // past the last tab: ignored
    assert_eq!(a.view, View::Mine);
}

#[test]
fn the_filter_narrows_as_you_type_and_esc_restores() {
    let mut a = sample_app();
    let mut i = crate::event::Input::default();
    a.set_view(View::All);
    press(&mut a, &mut i, "/tokens");
    assert!(a.filter_mode);
    assert_eq!(a.rows.len(), 1, "only the colour-tokens PR matches");
    // While typing, letters are text, not commands: `q` must not quit.
    press(&mut a, &mut i, "q");
    assert!(!a.quit && a.filter == "tokensq");
    press(&mut a, &mut i, "\x08\n");
    assert!(
        !a.filter_mode && a.filter == "tokens",
        "enter keeps the filter"
    );
    press(&mut a, &mut i, "\x1b");
    assert!(a.filter.is_empty());
    assert_eq!(a.rows.len(), 3);
}

#[test]
fn sort_drafts_and_help_toggle() {
    let mut a = sample_app();
    let mut i = crate::event::Input::default();
    a.set_view(View::All);
    press(&mut a, &mut i, "s");
    assert_eq!(a.sort, Sort::Attention);
    press(&mut a, &mut i, "s");
    assert_eq!(a.sort, Sort::Recent);

    let with_drafts = a.rows.len();
    press(&mut a, &mut i, "d");
    assert_eq!(a.rows.len(), with_drafts - 1, "the draft is hidden");
    press(&mut a, &mut i, "d");
    assert_eq!(a.rows.len(), with_drafts);

    // `q` closes help first; only a second `q` quits.
    press(&mut a, &mut i, "?");
    assert!(a.show_help);
    press(&mut a, &mut i, "j"); // other keys are swallowed while help is open
    assert_eq!(a.selected, 0);
    press(&mut a, &mut i, "q");
    assert!(!a.show_help && !a.quit);
    press(&mut a, &mut i, "q");
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
    a.show_help = true;
    insta::assert_snapshot!("help_120x30", render(&mut a, 120, 30));
    a.show_help = false;

    a.set_view(View::All);
    a.filter_mode = true;
    a.filter = "retry".into();
    a.rebuild();
    insta::assert_snapshot!("filtering_120x12", render(&mut a, 120, 12));
    a.filter_mode = false;
    a.filter = "zzz".into();
    a.rebuild();
    insta::assert_snapshot!("filter_matches_nothing_120x10", render(&mut a, 120, 10));
    a.filter.clear();
    a.rebuild();

    a.error = Some("gh api graphql: timed out after 45s".into());
    a.next_refresh = NOW + 180;
    insta::assert_snapshot!("sync_failed_120x10", render(&mut a, 120, 10));
    a.error = None;

    insta::assert_snapshot!("narrow_60x16", render(&mut a, 60, 16));
}
