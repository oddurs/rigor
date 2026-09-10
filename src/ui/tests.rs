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

fn sample_app() -> App {
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
            out.contains("CHECKS"),
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

/// The active view is marked by a heavy underline on the rail, spanning
/// exactly the tab above it — this is what makes the row read as a subnav,
/// and it survives NO_COLOR because it is a glyph, not a colour.
#[test]
fn the_rail_underlines_exactly_the_active_tab() {
    for view in [View::Ready, View::Blocked, View::Worktrees] {
        let mut a = sample_app();
        a.set_view(view);
        let buf = frame(&mut a, 160, 24);
        let (rect, _) = *a.hits.tabs.iter().find(|(_, v)| *v == view).unwrap();
        for x in 0..buf.area.width {
            let under = x >= rect.x && x < rect.x + rect.width;
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

/// Print a frame for eyeballing: `cargo test -- --nocapture preview`.
#[test]
fn preview() {
    let mut a = sample_app();
    a.set_view(View::All);
    println!(
        "\n=== 160x24 (side-by-side) ===\n{}",
        render(&mut a, 160, 24)
    );
    a.settings.layout = LayoutMode::Stack;
    println!("\n=== 100x24 (stacked) ===\n{}", render(&mut a, 100, 24));
    a.set_view(View::Worktrees);
    println!("\n=== worktrees 120x18 ===\n{}", render(&mut a, 120, 18));
    a.show_help = true;
    println!("\n=== help 120x28 ===\n{}", render(&mut a, 120, 28));
}
