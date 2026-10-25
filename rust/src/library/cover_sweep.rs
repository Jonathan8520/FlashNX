//! Réglages > Bibliothèque > Jaquettes manquantes: a cover for every game
//! that has none, taken from Flashpoint where the match is sure, and nowhere
//! else. Asked for by a player who had lost the covers folder.
//!
//! A game downloaded from Flashpoint knows its id (its `.url` holds the
//! GameZIP's address, which carries it): its logo is asked for straight away.
//! Any other game is searched for by its name, and only a game whose title,
//! or one of whose other titles, is that name and no other game's is taken
//! (`flashpoint::sure_match`). Then the logo, or the screenshot when there is
//! no logo, as the Jaquette picker does. A cover already there, made by hand
//! or picked, is never looked at: only the games without one are in the list.
//!
//! The requests go out on net.cpp's own thread, three at a time in the pool
//! the importer's thumbnails use (nothing else asks it while this page is up),
//! and the page reads what came back once per frame: the bar moves, B stops,
//! nothing waits on the network.

use super::*;
use crate::backend::render::{Btn, Choice, HintItem, Icon};

/// Requests in flight at once: three of the pool's four slots.
const PARALLEL: usize = 3;

/// The card is committed every few covers, and at the end: a commit per file
/// would cost more than the file.
const COMMIT_EVERY: usize = 8;

/// A game still to be looked at: its cover is cached under `basename`.
struct Job {
    basename: std::string::String,
    /// Its name as the library shows it, for the page.
    name: std::string::String,
    /// Its name as searched for (`cover_query_from_name`).
    query: std::string::String,
    /// Its `.swf`, beside which its `.url` may name its Flashpoint id.
    path: std::string::String,
}

enum Step {
    /// The search for the game's name.
    Search,
    /// The picture of Flashpoint game `id`: its logo, then (`shot`) its
    /// screenshot.
    Image { id: std::string::String, shot: bool },
}

struct Task {
    job: Job,
    step: Step,
    slot: i32,
}

struct Sweep {
    queue: std::collections::VecDeque<Job>,
    running: std::vec::Vec<Task>,
    total: usize,
    checked: usize,
    found: usize,
    /// Searches that brought no answer (the connection, the server): said at
    /// the end, apart from the games that have no sure match.
    unanswered: usize,
    uncommitted: usize,
    /// The game the page names: the last one a request went out for.
    current: std::string::String,
}

static SWEEP: Mutex<Option<Sweep>> = Mutex::new(None);

/// What the end window says: covers found, games looked for, searches with
/// no answer.
static TALLY: Mutex<(usize, usize, usize)> = Mutex::new((0, 0, 0));

/// The games without a cover, counted once per change of the library or of
/// a cover: (covers' generation, games, games without one).
static MISSING: Mutex<Option<(u32, usize, usize)>> = Mutex::new(None);

/// The games in `entries` that have no cover, one per cover name (games of
/// the same file name in two folders share one).
fn missing(entries: &[Entry]) -> std::vec::Vec<&Entry> {
    let mut seen = std::collections::BTreeSet::new();
    entries
        .iter()
        .filter(|e| seen.insert(e.basename.as_str()))
        .filter(|e| matches!(crate::covers::resolve(&e.basename), crate::covers::Cover::Default))
        .collect()
}

/// How many games have no cover, for the row. Answered from the scan's index
/// of the card (`covers::resolve`), and counted again only when a cover or
/// the list of games changed.
pub(crate) fn missing_count() -> usize {
    let generation = crate::covers::generation();
    let Ok(s) = LIBRARY.lock() else { return 0 };
    let games = s.entries.len();
    if let Some((g, n, m)) = MISSING.lock().ok().and_then(|g| *g) {
        if g == generation && n == games {
            return m;
        }
    }
    let m = missing(&s.entries).len();
    if let Ok(mut g) = MISSING.lock() {
        *g = Some((generation, games, m));
    }
    m
}

/// A on the row: the page, with every game without a cover in its list. With
/// none, or with no connection, a note says why nothing starts.
pub(crate) fn start(s: &mut State) {
    let jobs: std::collections::VecDeque<Job> = missing(&s.entries)
        .into_iter()
        .map(|e| Job {
            basename: e.basename.clone(),
            name: e.display_name.clone(),
            query: cover_query_from_name(&e.display_name),
            path: e.path.clone(),
        })
        .collect();
    if jobs.is_empty() {
        set_toast(s, crate::loc_ui::ui().cov_all_there.to_string(), TOAST_INFO);
        crate::sfx::hush();
        return;
    }
    if net::offline() {
        set_toast(s, crate::loc::s().err_no_internet.to_string(), TOAST_ERR);
        crate::sfx::choose(crate::sfx::Sfx::Error);
        return;
    }
    log(&std::format!("covers: sweep of {} game(s) without a cover\n", jobs.len()));
    crate::backend::render::thumb_cancel_all();
    let total = jobs.len();
    if let Ok(mut g) = SWEEP.lock() {
        *g = Some(Sweep {
            queue: jobs,
            running: std::vec::Vec::new(),
            total,
            checked: 0,
            found: 0,
            unanswered: 0,
            uncommitted: 0,
            current: std::string::String::new(),
        });
    }
    s.screen = Screen::CoverSweep;
}

/// The Flashpoint id the game was downloaded under, from its `.url`.
fn known_id(job: &Job) -> Option<std::string::String> {
    let url = read_sd_text(&std::format!("{}.url", job.path), 4096).ok()?;
    crate::sources::flashpoint::id_in_source_url(&url)
}

/// Send the request `step` of `job` makes. Gives the job back when the pool
/// has no free slot.
fn send(sw: &mut Sweep, job: Job, step: Step) -> Result<(), Job> {
    let url = match &step {
        Step::Search => match crate::sources::flashpoint::sure_match_url(&job.query) {
            Some(u) => u,
            None => {
                // No word to search for: nothing sure can come of it.
                sw.checked += 1;
                return Ok(());
            }
        },
        Step::Image { id, shot } => {
            let logo = crate::sources::flashpoint::cover_url(id);
            if *shot {
                logo.replace("/Logos/", "/Screenshots/")
            } else {
                logo
            }
        }
    };
    let slot = net::thumb_start(&url);
    if slot < 0 {
        return Err(job);
    }
    sw.current = job.name.clone();
    sw.running.push(Task { job, step, slot });
    Ok(())
}

/// Once per frame, before the screen is read: what came back, and the next
/// requests. One picture is checked and written per frame (a decode is a few
/// milliseconds); the others wait in their slots.
pub(crate) fn drive() {
    let Ok(mut g) = SWEEP.lock() else { return };
    let Some(sw) = g.as_mut() else { return };
    let mut pictured = false;
    let mut i = 0;
    while i < sw.running.len() {
        let slot = sw.running[i].slot;
        let status = net::thumb_slot_status(slot);
        if status == 0 || (pictured && matches!(sw.running[i].step, Step::Image { .. })) {
            i += 1;
            continue;
        }
        let Task { job, step, .. } = sw.running.remove(i);
        let body = if status > 0 { net::thumb_slot_take(slot) } else { None };
        if status < 0 {
            // Freed or failed: `take` frees it either way.
            let _ = net::thumb_slot_take(slot);
        }
        match step {
            Step::Search => match body {
                Some(json) => match crate::sources::flashpoint::sure_match(&json, &job.query) {
                    Some(id) => {
                        if let Err(job) = send(sw, job, Step::Image { id, shot: false }) {
                            // The pool is the sweep's: a full one comes back
                            // free on a later frame.
                            sw.queue.push_front(job);
                        }
                    }
                    None => {
                        log(&std::format!("covers: sweep \"{}\": no sure match\n", job.query));
                        sw.checked += 1;
                    }
                },
                None => {
                    log(&std::format!("covers: sweep \"{}\": search had no answer\n", job.query));
                    sw.unanswered += 1;
                    sw.checked += 1;
                }
            },
            Step::Image { id, shot } => {
                pictured = true;
                match body.map(|b| crate::covers::cache_bytes(&job.basename, &b)) {
                    Some(Ok(path)) => {
                        log(&std::format!("covers: sweep \"{}\" -> {}\n", job.basename, path));
                        crate::backend::render::invalidate_cover(&job.basename);
                        sw.found += 1;
                        sw.checked += 1;
                        sw.uncommitted += 1;
                        if sw.uncommitted >= COMMIT_EVERY {
                            crate::sd::commit();
                            sw.uncommitted = 0;
                        }
                    }
                    _ if !shot => {
                        if let Err(job) = send(sw, job, Step::Image { id, shot: true }) {
                            sw.queue.push_front(job);
                        }
                    }
                    _ => {
                        log(&std::format!("covers: sweep \"{}\": no picture for {}\n", job.basename, id));
                        sw.checked += 1;
                    }
                }
            }
        }
    }
    while sw.running.len() < PARALLEL {
        let Some(job) = sw.queue.pop_front() else { break };
        let step = match known_id(&job) {
            Some(id) => Step::Image { id, shot: false },
            None => Step::Search,
        };
        if let Err(job) = send(sw, job, step) {
            sw.queue.push_front(job);
            break;
        }
    }
    if sw.queue.is_empty() && sw.running.is_empty() {
        let done = (sw.found, sw.total, sw.unanswered);
        *g = None;
        drop(g);
        finish(done);
    }
}

/// The sweep over, run to its end or stopped: the card committed, the count
/// of games without a cover taken again, and the window that says how it went.
fn finish(tally: (usize, usize, usize)) {
    crate::sd::commit();
    log(&std::format!("covers: sweep over, {} found of {}, {} unanswered\n", tally.0, tally.1, tally.2));
    if let Ok(mut t) = TALLY.lock() {
        *t = tally;
    }
    if let Ok(mut s) = LIBRARY.lock() {
        if matches!(s.screen, Screen::CoverSweep) {
            s.screen = Screen::CoverSweepDone;
        }
    }
}

/// B on the page: what is in flight is dropped, what was found stays.
pub(crate) fn page_input(s: &mut State, button: &str) {
    if !matches!(button, "B" | "Minus") {
        return;
    }
    let tally = SWEEP.lock().ok().and_then(|mut g| g.take()).map(|sw| (sw.found, sw.total, sw.unanswered));
    crate::backend::render::thumb_cancel_all();
    crate::sd::commit();
    if let Some(t) = tally {
        log(&std::format!("covers: sweep stopped, {} found of {}\n", t.0, t.1));
        if let Ok(mut g) = TALLY.lock() {
            *g = t;
        }
    }
    s.screen = Screen::CoverSweepDone;
}

/// The end window: A or B closes it, on the row it came from.
pub(crate) fn done_input(s: &mut State, button: &str) {
    if matches!(button, "A" | "B" | "Minus") {
        s.screen = settings_at(SetSection::Library, "JAQUETTES");
    }
}

/// The page: the game being looked for, a bar, how many are done and found.
pub(crate) fn draw_page(backend: &mut SwitchRenderBackend) {
    let u = crate::loc_ui::ui();
    let (name, checked, total, found) = SWEEP
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|sw| (sw.current.clone(), sw.checked, sw.total, sw.found)))
        .unwrap_or_default();
    let frac = if total == 0 { 0.0 } else { checked as f32 / total as f32 };
    let detail = u
        .cov_progress
        .replacen("{}", &checked.to_string(), 1)
        .replacen("{}", &total.to_string(), 1)
        .replacen("{}", &found.to_string(), 1);
    let hints: [HintItem; 1] = [(&[Btn::B], u.imp_stop)];
    backend.draw_progress_page(Icon::Image, u.cov_page_title, &name, Some(frac), &detail, &hints);
}

/// The end window, over the Bibliothèque pane.
pub(crate) fn draw_done(backend: &mut SwitchRenderBackend) {
    let u = crate::loc_ui::ui();
    let (found, total, unanswered) = TALLY.lock().map(|t| *t).unwrap_or_default();
    let title = u.cov_done_title.replacen("{}", &found.to_string(), 1).replacen("{}", &total.to_string(), 1);
    let detail = (unanswered > 0).then(|| u.cov_no_answer.replacen("{}", &unanswered.to_string(), 1));
    backend.draw_confirm(
        if found > 0 { Icon::CircleCheck } else { Icon::Image },
        &title,
        u.cov_done_body,
        detail.as_deref(),
        &[Choice { label: u.close, destructive: false }],
        0,
        false,
        u.close,
    );
}
