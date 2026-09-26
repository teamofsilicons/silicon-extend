//! The one thread that owns every COM object and carries out Windows commands in order.

// An `Err` here is the command's finished answer (a failed `Output`), built once per command.
#![allow(clippy::result_large_err)]

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use bridge_driver::{LocalFile, Output};
use bridge_protocol::model::{CommandError, FileKind};
use serde_json::{Value, json};
use windows::Win32::UI::Accessibility::IUIAutomationElement;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};

use super::apps::{self, StartApp};
use super::commands::{self, Action, FindAction, Locator, Predicate, Surface, Target, Wait};
use super::image;
use super::keys;
use super::model::{RawNode, Snapshot, SnapshotOptions, node_json};
use super::selector::{Key, Selector, Term};
use super::uia::{self, Capture, TopWindow, Uia};
use super::{capture, clipboard, input, shell};

pub enum Request {
    Run(Job),
    EndSession(String),
}

pub struct Job {
    pub session: String,
    pub action: Action,
    pub workdir: PathBuf,
    pub cancel: Arc<AtomicBool>,
    pub deadline: Instant,
    pub reply: tokio::sync::oneshot::Sender<Output>,
}

/// Starts the worker thread and returns its queue.
pub fn spawn() -> Sender<Request> {
    let (tx, rx) = channel();
    let _ = std::thread::Builder::new().name("bridge-windows-uia".into()).spawn(move || run(rx));
    tx
}

fn run(rx: Receiver<Request>) {
    // Physical pixels everywhere, so UI Automation rectangles, the pointer and captures agree.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let mut worker = Worker { uia: Uia::new(), sessions: HashMap::new(), apps: None };
    for request in rx {
        match request {
            Request::Run(job) => {
                let out = catch_unwind(AssertUnwindSafe(|| worker.execute(&job))).unwrap_or_else(|_| {
                    Output::fail("internal", "The Windows driver hit an unexpected error running this command.")
                });
                let _ = job.reply.send(out);
            }
            Request::EndSession(id) => {
                worker.sessions.remove(&id);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct AppTarget {
    name: String,
    id: String,
    pid: Option<u32>,
}

struct Last {
    cap: Capture,
    snapshot: Snapshot,
}

struct Session {
    surface: Surface,
    app: Option<AppTarget>,
    last: Option<Last>,
}

impl Default for Session {
    fn default() -> Self {
        Self { surface: Surface::FrontmostApp, app: None, last: None }
    }
}

/// What a target resolved to.
struct Resolved {
    el: Option<IUIAutomationElement>,
    node: Option<RawNode>,
    reference: Option<String>,
    point: Option<(i32, i32)>,
}

impl Resolved {
    fn describe(&self) -> String {
        let label = self.node.as_ref().map(RawNode::display_label).unwrap_or_default();
        match (&self.reference, &self.node, self.point) {
            (Some(r), _, _) if !label.is_empty() => format!("@{r} \"{label}\""),
            (Some(r), _, _) => format!("@{r}"),
            (None, Some(n), _) => format!("{} \"{label}\"", n.role()),
            (None, None, Some((x, y))) => format!("{x},{y}"),
            _ => "the element".into(),
        }
    }
}

fn fail_with(code: &str, message: impl Into<String>, details: Value) -> Output {
    let message = message.into();
    Output {
        ok: false,
        output: Value::Null,
        text: Some(message.clone()),
        error: Some(CommandError { code: code.into(), message, details }),
        files: vec![],
    }
}

fn contains_ci(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(&needle.to_lowercase())
}

fn exe_stem(path_or_id: &str) -> String {
    let file = path_or_id.rsplit(['\\', '/']).next().unwrap_or(path_or_id);
    let lower = file.to_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_owned()
}

struct Worker {
    uia: Result<Uia, String>,
    sessions: HashMap<String, Session>,
    apps: Option<(Instant, Vec<StartApp>)>,
}

impl Worker {
    fn session(&mut self, id: &str) -> &mut Session {
        self.sessions.entry(id.to_owned()).or_default()
    }

    fn uia(&self) -> Result<&Uia, Output> {
        self.uia.as_ref().map_err(|e| Output::fail("unsupported_on_device", e.clone()))
    }

    fn execute(&mut self, job: &Job) -> Output {
        match self.dispatch(job) {
            Ok(out) | Err(out) => out,
        }
    }

    fn dispatch(&mut self, job: &Job) -> Result<Output, Output> {
        let sid = job.session.as_str();
        match &job.action {
            Action::Snapshot(opts) => {
                let last = self.capture(sid, opts, &job.cancel)?;
                Ok(Output::ok(last.snapshot.to_json(&last.cap.raw), last.snapshot.to_text(&last.cap.raw)))
            }
            Action::GetText(t) => {
                let r = self.resolve(sid, t, &job.cancel)?;
                let node = r.node.clone().ok_or_else(|| Output::fail("invalid_args", "A point has no text; give a ref or a selector."))?;
                let text = match node.role() {
                    "text-field" | "text-view" => node.value.clone().filter(|v| !v.is_empty()).unwrap_or(node.name.clone()),
                    _ => if node.name.is_empty() { node.value.clone().unwrap_or_default() } else { node.name.clone() },
                };
                Ok(Output::ok(json!({"ref": r.reference.as_ref().map(|x| format!("@{x}")), "text": text}), text))
            }
            Action::GetAttrs(t) => {
                let r = self.resolve(sid, t, &job.cancel)?;
                let node = r.node.clone().ok_or_else(|| Output::fail("invalid_args", "A point has no attributes; give a ref or a selector."))?;
                let attrs = node_json(&node, r.reference.as_deref().unwrap_or(""), 0, node.depth, None);
                let text = serde_json::to_string_pretty(&attrs).unwrap_or_default();
                Ok(Output::ok(attrs, text))
            }
            Action::Find { locator, query, action } => self.find(job, *locator, query, action),
            Action::Is { predicate, target, value } => self.is(job, *predicate, target, value.as_deref()),
            Action::Wait(w) => self.wait(job, w),
            Action::Screenshot { name, scale, fullscreen } => self.screenshot(job, name, *scale, *fullscreen),
            Action::Click { target, button, count, interval_ms } => {
                let r = self.resolve(sid, target, &job.cancel)?;
                let (x, y) = self.point_of(&r)?;
                input::click(x, y, *button, *count, *interval_ms).map_err(|e| Output::fail("command_failed", e))?;
                let what = r.describe();
                let verb = if *count > 1 { format!("Clicked {count} times") } else { "Clicked".into() };
                Ok(Output::ok(json!({"ref": r.reference.map(|x| format!("@{x}")), "x": x, "y": y}), format!("{verb} {what}")))
            }
            Action::Hover(target) => {
                let r = self.resolve(sid, target, &job.cancel)?;
                let (x, y) = self.point_of(&r)?;
                input::move_to(x, y).map_err(|e| Output::fail("command_failed", e))?;
                Ok(Output::ok(json!({"x": x, "y": y}), format!("Hovered {}", r.describe())))
            }
            Action::Focus(target) => {
                let r = self.resolve(sid, target, &job.cancel)?;
                self.focus(&r)?;
                Ok(Output::ok(json!({"ref": r.reference.as_ref().map(|x| format!("@{x}"))}), format!("Focused {}", r.describe())))
            }
            Action::Fill { target, text } => {
                let r = self.resolve(sid, target, &job.cancel)?;
                self.fill(&r, text, &job.cancel)?;
                Ok(Output::ok(json!({"ref": r.reference.as_ref().map(|x| format!("@{x}")), "length": text.chars().count()}), format!("Filled {}", r.describe())))
            }
            Action::Type(text) => {
                input::keystrokes(&keys::plan(text), &job.cancel).map_err(|e| Output::fail("command_failed", e))?;
                let n = text.chars().count();
                Ok(Output::ok(json!({"length": n}), format!("Typed {n} characters")))
            }
            Action::Scroll { direction, fraction, pixels } => {
                let (x, y) = self.scroll_point(sid);
                let (v, h) = commands::wheel_delta(*direction, *fraction, *pixels);
                input::wheel(x, y, v, h).map_err(|e| Output::fail("command_failed", e))?;
                Ok(Output::ok(json!({"x": x, "y": y, "wheelDelta": v, "wheelDeltaHorizontal": h}), "Scrolled".to_owned()))
            }
            Action::ClipboardRead => {
                let text = clipboard::read().map_err(|e| Output::fail("command_failed", e))?;
                Ok(Output::ok(json!({"platform": "windows", "action": "read", "text": text}), text))
            }
            Action::ClipboardWrite(text) => {
                clipboard::write(text).map_err(|e| Output::fail("command_failed", e))?;
                Ok(Output::ok(json!({"platform": "windows", "action": "write"}), "Clipboard updated".to_owned()))
            }
            Action::Open { target, surface } => self.open(job, target.as_deref(), *surface),
            Action::Close { app } => self.close(sid, app.as_deref()),
            Action::Apps { all } => {
                let list = self.start_apps()?;
                let shown = apps::visible_apps(&list, *all);
                let names: Vec<String> = shown.iter().map(|a| format!("{} ({})", a.name, a.app_id)).collect();
                Ok(Output::ok(json!({"apps": names}), apps::apps_text(&shown, *all)))
            }
            Action::AppState => {
                let fg = uia::foreground_window().ok_or_else(|| Output::fail("command_failed", "No window is in front."))?;
                let name = uia::process_stem(fg.pid).unwrap_or_default();
                let path = uia::process_path(fg.pid).unwrap_or_default();
                let surface = self.session(sid).surface.as_str();
                Ok(Output::ok(
                    json!({"platform": "windows", "appName": name, "appBundleId": path, "windowTitle": fg.title, "pid": fg.pid, "surface": surface, "source": "foreground"}),
                    format!("{name} is in front ({})", fg.title),
                ))
            }
        }
    }

    // ───────────── windows and captures ─────────────

    /// The windows a session looks at, with the app name and id to report.
    fn session_windows(&mut self, sid: &str) -> Result<(Vec<TopWindow>, String, String), Output> {
        let own = std::process::id();
        let session = self.session(sid);
        let surface = session.surface;
        let app = session.app.clone();
        let all: Vec<TopWindow> = uia::top_windows().into_iter().filter(|w| w.pid != own).collect();
        match (surface, app) {
            (Surface::Desktop, _) => Ok((all.into_iter().take(24).collect(), "Desktop".into(), "desktop".into())),
            (Surface::App, Some(mut app)) => {
                let wins = windows_of_app(&all, &mut app);
                if wins.is_empty() {
                    return Err(Output::fail(
                        "app_not_running",
                        format!("{} has no open windows. Open it again with `bridge open {}`.", app.name, app.name),
                    ));
                }
                let id = if app.id.is_empty() { wins.first().and_then(|w| uia::process_path(w.pid)).unwrap_or_default() } else { app.id.clone() };
                let name = app.name.clone();
                self.session(sid).app = Some(app);
                Ok((wins, name, id))
            }
            _ => {
                let fg = uia::foreground_window().filter(|w| w.pid != own).or_else(|| all.first().cloned());
                let Some(fg) = fg else { return Err(Output::fail("command_failed", "No app window is open on this computer.")) };
                let wins: Vec<TopWindow> = all.into_iter().filter(|w| w.pid == fg.pid).collect();
                let wins = if wins.is_empty() { vec![fg.clone()] } else { wins };
                let name = uia::process_stem(fg.pid).unwrap_or_else(|| fg.title.clone());
                let id = uia::process_path(fg.pid).unwrap_or_default();
                Ok((wins, name, id))
            }
        }
    }

    /// Captures the session's windows, builds a snapshot, and makes it the session's latest.
    fn capture(&mut self, sid: &str, opts: &SnapshotOptions, cancel: &AtomicBool) -> Result<&Last, Output> {
        let (wins, name, id) = self.session_windows(sid)?;
        let cap = self.uia()?.capture(&wins, &name, &id, cancel);
        let mut opts = opts.clone();
        // `-s @e3` scopes by that element's label in the previous snapshot.
        if let Some(scope) = opts.scope.clone().filter(|s| s.starts_with('@')) {
            let label = self.session(sid).last.as_ref().and_then(|l| {
                l.snapshot.resolve_ref(&scope).map(|i| l.cap.raw[i].display_label())
            });
            match label {
                Some(l) if !l.is_empty() => opts.scope = Some(l),
                _ => return Err(Output::fail("invalid_args", format!("{scope} isn't in the latest snapshot. Take a new snapshot."))),
            }
        }
        let snapshot = super::model::build_snapshot(&cap.raw, &opts, &name, &id, cap.truncated);
        let session = self.session(sid);
        session.last = Some(Last { cap, snapshot });
        Ok(session.last.as_ref().expect("just set"))
    }

    fn resolve(&mut self, sid: &str, target: &Target, cancel: &AtomicBool) -> Result<Resolved, Output> {
        match target {
            Target::Point(x, y) => Ok(Resolved { el: None, node: None, reference: None, point: Some((*x, *y)) }),
            Target::Ref(r) => {
                let session = self.session(sid);
                let Some(last) = session.last.as_ref() else {
                    return Err(Output::fail("invalid_args", format!("{r} needs a snapshot first: refs come from the latest `bridge snapshot` in this session.")));
                };
                let Some(i) = last.snapshot.resolve_ref(r) else {
                    return Err(Output::fail(
                        "invalid_args",
                        format!("{r} isn't in the latest snapshot (it has refs @e1 to @e{}). Take a new snapshot.", last.snapshot.nodes.len()),
                    ));
                };
                let el = last.cap.elements[i].clone();
                let cached = last.cap.raw[i].clone();
                let reference = last.snapshot.ref_of(i).map(str::to_owned);
                let node = match (&el, self.uia.as_ref()) {
                    (Some(e), Ok(u)) => u.refresh(e).map(|mut n| {
                        n.depth = cached.depth;
                        n.app_name = cached.app_name.clone();
                        n.app_id = cached.app_id.clone();
                        n.window_title = cached.window_title.clone();
                        n
                    }),
                    _ => Some(cached),
                };
                if node.is_none() {
                    return Err(Output::fail("stale_ref", format!("{r} is gone from the screen. Take a new snapshot.")));
                }
                Ok(Resolved { el, node, reference, point: None })
            }
            Target::Selector(sel) => {
                let last = self.capture(sid, &SnapshotOptions::default(), cancel)?;
                let hits: Vec<usize> = sel.find_all(&last.cap.raw).into_iter().filter(|&i| last.cap.elements[i].is_some()).collect();
                let Some(&i) = hits.first() else {
                    return Err(fail_with(
                        "not_found",
                        format!("Nothing on screen matches {}. Take a snapshot to see what's there.", sel.source),
                        json!({"selector": sel.source}),
                    ));
                };
                Ok(Resolved {
                    el: last.cap.elements[i].clone(),
                    node: Some(last.cap.raw[i].clone()),
                    reference: last.snapshot.ref_of(i).map(str::to_owned),
                    point: None,
                })
            }
        }
    }

    fn point_of(&self, r: &Resolved) -> Result<(i32, i32), Output> {
        if let Some(p) = r.point {
            return Ok(p);
        }
        let rect = r.el.as_ref().and_then(uia::current_rect).or_else(|| r.node.as_ref().and_then(|n| n.rect));
        match rect {
            Some(rect) if !r.node.as_ref().is_some_and(|n| n.offscreen) => Ok(rect.center()),
            _ => Err(Output::fail(
                "not_visible",
                format!("{} isn't on screen. Scroll it into view and take a new snapshot.", r.describe()),
            )),
        }
    }

    fn focus(&self, r: &Resolved) -> Result<(), Output> {
        if let Some(el) = &r.el
            && uia::set_focus(el).is_ok()
        {
            return Ok(());
        }
        let (x, y) = self.point_of(r)?;
        input::click(x, y, commands::Button::Primary, 1, 0).map_err(|e| Output::fail("command_failed", e))
    }

    fn fill(&self, r: &Resolved, text: &str, cancel: &AtomicBool) -> Result<(), Output> {
        let focused = match &r.el {
            Some(el) => uia::set_focus(el).is_ok(),
            None => false,
        };
        if !focused {
            if let Some(el) = &r.el
                && uia::set_value(el, text).is_ok()
            {
                return Ok(());
            }
            let (x, y) = self.point_of(r)?;
            input::click(x, y, commands::Button::Primary, 1, 0).map_err(|e| Output::fail("command_failed", e))?;
        }
        std::thread::sleep(Duration::from_millis(60));
        let mut strokes = keys::clear_field();
        strokes.extend(keys::plan(text));
        input::keystrokes(&strokes, cancel).map_err(|e| Output::fail("command_failed", e))
    }

    fn scroll_point(&mut self, sid: &str) -> (i32, i32) {
        let win = self.session_windows(sid).ok().and_then(|(w, _, _)| w.into_iter().find(|w| !w.minimized));
        match win {
            Some(w) => w.rect.center(),
            None => capture::virtual_screen().center(),
        }
    }

    fn remaining(job: &Job, wanted: Duration) -> Duration {
        wanted.min(job.deadline.saturating_duration_since(Instant::now()))
    }

    /// Sleeps up to `d`, waking early on cancel.
    fn nap(job: &Job, d: Duration) -> bool {
        let end = Instant::now() + d;
        while Instant::now() < end {
            if job.cancel.load(Ordering::Relaxed) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25).min(end.saturating_duration_since(Instant::now())));
        }
        !job.cancel.load(Ordering::Relaxed)
    }

    // ───────────── queries ─────────────

    fn find(&mut self, job: &Job, locator: Locator, query: &str, action: &FindAction) -> Result<Output, Output> {
        let q = query.to_owned();
        let term = match locator {
            Locator::Any => Term::Text(q),
            Locator::Text => Term::Contains(Key::Text, vec![q.to_lowercase()]),
            Locator::Label => Term::Contains(Key::Label, vec![q.to_lowercase()]),
            Locator::Value => Term::Contains(Key::Value, vec![q.to_lowercase()]),
            Locator::Role => Term::Equals(Key::Role, q),
            Locator::Id => Term::Equals(Key::Id, q),
        };
        let sel = Selector { alternatives: vec![vec![term]], source: query.to_owned() };
        let sid = job.session.as_str();
        let wait_until = match action {
            FindAction::Wait(ms) => Some(Instant::now() + Self::remaining(job, Duration::from_millis(*ms))),
            _ => None,
        };
        loop {
            let last = self.capture(sid, &SnapshotOptions::default(), &job.cancel)?;
            let hits: Vec<usize> = sel.find_all(&last.cap.raw).into_iter().filter(|&i| i > 0).collect();
            let lines: Vec<String> = hits
                .iter()
                .filter_map(|&i| {
                    let r = last.snapshot.ref_of(i)?;
                    let n = &last.cap.raw[i];
                    Some(format!("@{r} [{}] \"{}\"", n.role(), n.display_label()))
                })
                .collect();
            let refs: Vec<String> = hits.iter().filter_map(|&i| last.snapshot.ref_of(i).map(|r| format!("@{r}"))).collect();
            if hits.is_empty() {
                if let Some(until) = wait_until
                    && Instant::now() < until
                    && Self::nap(job, Duration::from_millis(300))
                {
                    continue;
                }
                return Err(fail_with(
                    "not_found",
                    format!("Nothing on screen matches {query:?} ({}).", locator.as_str()),
                    json!({"query": query, "locator": locator.as_str()}),
                ));
            }
            let first = hits[0];
            let resolved = Resolved {
                el: last.cap.elements[first].clone(),
                node: Some(last.cap.raw[first].clone()),
                reference: last.snapshot.ref_of(first).map(str::to_owned),
                point: None,
            };
            let base = json!({"ref": refs.first(), "locator": locator.as_str(), "query": query, "matches": refs});
            return match action {
                FindAction::List | FindAction::Wait(_) => Ok(Output::ok(base, lines.join("\n"))),
                FindAction::Click => {
                    let (x, y) = self.point_of(&resolved)?;
                    input::click(x, y, commands::Button::Primary, 1, 0).map_err(|e| Output::fail("command_failed", e))?;
                    let mut out = base;
                    out["x"] = json!(x);
                    out["y"] = json!(y);
                    Ok(Output::ok(out, format!("Clicked {}", resolved.describe())))
                }
                FindAction::Focus => {
                    self.focus(&resolved)?;
                    Ok(Output::ok(base, format!("Focused {}", resolved.describe())))
                }
                FindAction::Fill(text) => {
                    self.fill(&resolved, text, &job.cancel)?;
                    Ok(Output::ok(base, format!("Filled {}", resolved.describe())))
                }
            };
        }
    }

    /// Matching nodes for a target, from a fresh look at the screen.
    fn matches_now(&mut self, sid: &str, target: &Target, cancel: &AtomicBool) -> Result<Vec<RawNode>, Output> {
        match target {
            Target::Point(..) => Err(Output::fail("invalid_args", "Give a ref or a selector, not a point.")),
            Target::Ref(_) => match self.resolve(sid, target, cancel) {
                Ok(r) => Ok(r.node.into_iter().collect()),
                Err(e) if e.error.as_ref().is_some_and(|x| x.code == "stale_ref") => Ok(vec![]),
                Err(e) => Err(e),
            },
            Target::Selector(sel) => {
                let last = self.capture(sid, &SnapshotOptions::default(), cancel)?;
                Ok(sel.find_all(&last.cap.raw).into_iter().filter(|&i| i > 0).map(|i| last.cap.raw[i].clone()).collect())
            }
        }
    }

    fn is(&mut self, job: &Job, predicate: Predicate, target: &Target, value: Option<&str>) -> Result<Output, Output> {
        let nodes = self.matches_now(&job.session, target, &job.cancel)?;
        let first = nodes.first();
        let (pass, why) = match predicate {
            Predicate::Exists => (!nodes.is_empty(), format!("{} match(es)", nodes.len())),
            Predicate::Absent => (nodes.is_empty(), format!("{} match(es)", nodes.len())),
            Predicate::Visible => (nodes.iter().any(RawNode::visible), "visibility".into()),
            Predicate::Hidden => (!nodes.iter().any(RawNode::visible), "visibility".into()),
            Predicate::Editable => (first.is_some_and(|n| n.editable), "editable".into()),
            Predicate::Selected => (first.is_some_and(|n| n.selected == Some(true)), "selected".into()),
            Predicate::Focused => (first.is_some_and(|n| n.focused), "focused".into()),
            Predicate::Text => {
                let want = value.unwrap_or("").trim();
                let got = first.map(|n| {
                    if n.name.trim() == want || n.value.as_deref().map(str::trim) == Some(want) { want.to_owned() } else { n.display_label() }
                });
                (got.as_deref() == Some(want), format!("text is {:?}", got.unwrap_or_default()))
            }
        };
        let name = format!("{predicate:?}").to_lowercase();
        let details = json!({"pass": pass, "predicate": name, "target": target.describe(), "reason": why});
        if pass {
            Ok(Output::ok(details, format!("pass: {name} {}", target.describe())))
        } else {
            Err(fail_with("assertion_failed", format!("fail: {name} {} ({why})", target.describe()), details))
        }
    }

    fn wait(&mut self, job: &Job, w: &Wait) -> Result<Output, Output> {
        let start = Instant::now();
        let (ms, present, target): (u64, bool, Option<Target>) = match w {
            Wait::Ms(ms) => {
                let d = Self::remaining(job, Duration::from_millis(*ms));
                if !Self::nap(job, d) {
                    return Err(Output::fail("cancelled", "The wait was cancelled."));
                }
                return Ok(Output::ok(json!({"waitedMs": start.elapsed().as_millis() as u64}), format!("Waited {} ms", d.as_millis())));
            }
            Wait::Text(t, ms) => (*ms, true, Some(Target::Selector(Selector { alternatives: vec![vec![Term::Text(t.clone())]], source: t.clone() }))),
            Wait::Present(t, ms) => (*ms, true, Some(self.wait_target(&job.session, t))),
            Wait::Absent(t, ms) => (*ms, false, Some(t.clone())),
        };
        let target = target.expect("set above");
        let until = start + Self::remaining(job, Duration::from_millis(ms));
        loop {
            let found = !self.matches_now(&job.session, &target, &job.cancel)?.is_empty();
            if found == present {
                let waited = start.elapsed().as_millis() as u64;
                let what = if present { "appeared" } else { "went away" };
                return Ok(Output::ok(json!({"waitedMs": waited, "target": target.describe()}), format!("{} {what} after {waited} ms", target.describe())));
            }
            if Instant::now() >= until || !Self::nap(job, Duration::from_millis(300)) {
                let reason = if present { "wait_target_absent" } else { "wait_target_present" };
                return Err(fail_with(
                    "command_failed",
                    format!("{} {} after {} ms.", target.describe(), if present { "never appeared" } else { "was still there" }, start.elapsed().as_millis()),
                    json!({"reason": reason, "waitedMs": start.elapsed().as_millis() as u64}),
                ));
            }
        }
    }

    /// `wait @e3` waits for that element's label to be on screen (refs are stale once the screen changes).
    fn wait_target(&mut self, sid: &str, t: &Target) -> Target {
        if let Target::Ref(r) = t
            && let Some(label) = self.session(sid).last.as_ref().and_then(|l| l.snapshot.resolve_ref(r).map(|i| l.cap.raw[i].display_label()))
            && !label.is_empty()
        {
            return Target::Selector(Selector { alternatives: vec![vec![Term::Text(label.clone())]], source: format!("{r} \"{label}\"") });
        }
        t.clone()
    }

    // ───────────── evidence ─────────────

    fn screenshot(&mut self, job: &Job, name: &str, scale: Option<f64>, fullscreen: bool) -> Result<Output, Output> {
        let sid = job.session.as_str();
        let surface = self.session(sid).surface;
        let rect = if fullscreen || surface != Surface::App {
            capture::virtual_screen()
        } else {
            match self.session_windows(sid) {
                Ok((wins, _, _)) => wins.iter().find(|w| !w.minimized).map(|w| w.rect).unwrap_or_else(capture::virtual_screen),
                Err(_) => capture::virtual_screen(),
            }
        };
        let screen = capture::virtual_screen();
        let (full, sw, sh) = capture::grab(screen).map_err(|e| Output::fail("command_failed", e))?;
        let (rgba, w, h) = if rect == screen {
            (full, sw, sh)
        } else {
            let (x, y) = ((rect.x - screen.x) as i32, (rect.y - screen.y) as i32);
            image::crop_rgba(&full, sw, sh, x, y, rect.width as i32, rect.height as i32)
        };
        let (rgba, w, h) = match scale {
            Some(s) => image::scale_rgba(&rgba, w, h, s),
            None => (rgba, w, h),
        };
        let path = job.workdir.join(name);
        capture::write_png(&path, &rgba, w, h).map_err(|e| Output::fail("command_failed", e))?;
        let out = Output::ok(json!({"path": path.display().to_string(), "width": w, "height": h}), format!("{} ({w}x{h})", path.display()));
        Ok(out.with_file(LocalFile { path, name: name.to_owned(), content_type: "image/png".into(), kind: FileKind::Screenshot }))
    }

    // ───────────── apps ─────────────

    fn start_apps(&mut self) -> Result<Vec<StartApp>, Output> {
        if let Some((at, list)) = &self.apps
            && at.elapsed() < Duration::from_secs(60)
        {
            return Ok(list.clone());
        }
        let list = shell::start_apps().map_err(|e| Output::fail("command_failed", e))?;
        self.apps = Some((Instant::now(), list.clone()));
        Ok(list)
    }

    fn open(&mut self, job: &Job, target: Option<&str>, surface: Surface) -> Result<Output, Output> {
        let sid = job.session.as_str();
        let Some(target) = target else {
            let s = self.session(sid);
            s.surface = surface;
            s.app = None;
            s.last = None;
            let text = match surface {
                Surface::Desktop => "Opened: desktop".to_owned(),
                _ => {
                    let name = uia::foreground_window().and_then(|w| uia::process_stem(w.pid)).unwrap_or_default();
                    format!("Opened: {name} (frontmost app)")
                }
            };
            return Ok(Output::ok(json!({"platform": "windows", "surface": surface.as_str(), "message": text}), text));
        };
        if commands::is_url(target) {
            shell::shell_open(target).map_err(|e| Output::fail("command_failed", e))?;
            let text = format!("Opened: {target}");
            return Ok(Output::ok(json!({"platform": "windows", "url": target, "message": text}), text));
        }
        let own = std::process::id();
        let before: HashSet<isize> = uia::top_windows().iter().map(|w| w.hwnd.0 as isize).collect();
        let looks_like_path = target.contains('\\') || target.contains('/') || target.to_lowercase().ends_with(".exe");
        let (name, id) = if looks_like_path {
            shell::shell_open(target).map_err(|e| Output::fail("command_failed", e))?;
            (exe_stem(target), target.to_owned())
        } else {
            let list = self.start_apps()?;
            let Some(app) = apps::resolve(&list, target).cloned() else {
                let close = apps::suggestions(&list, target, 5);
                let hint = if close.is_empty() { String::new() } else { format!(" Close matches: {}.", close.join(", ")) };
                return Err(fail_with(
                    "app_not_found",
                    format!("No app named {target:?} in the Start menu.{hint} Run `bridge apps --all` to list them."),
                    json!({"query": target, "suggestions": close}),
                ));
            };
            shell::launch_app(&app).map_err(|e| Output::fail("command_failed", e))?;
            (app.name.clone(), app.app_id.clone())
        };
        let mut app = AppTarget { name: name.clone(), id: id.clone(), pid: None };
        let until = Instant::now() + Self::remaining(job, Duration::from_secs(10));
        let mut found: Option<TopWindow> = None;
        while Instant::now() < until {
            let all: Vec<TopWindow> = uia::top_windows().into_iter().filter(|w| w.pid != own).collect();
            let fresh = all.iter().find(|w| !before.contains(&(w.hwnd.0 as isize)));
            if let Some(w) = fresh {
                found = Some(w.clone());
                break;
            }
            let mut probe = app.clone();
            if let Some(w) = windows_of_app(&all, &mut probe).into_iter().next() {
                shell::bring_to_front(w.hwnd);
                found = Some(w);
                break;
            }
            if !Self::nap(job, Duration::from_millis(250)) {
                break;
            }
        }
        app.pid = found.as_ref().map(|w| w.pid);
        let s = self.session(sid);
        s.surface = Surface::App;
        s.app = Some(app);
        s.last = None;
        let text = match &found {
            Some(_) => format!("Opened: {name}"),
            None => format!("Opened: {name} (no window has appeared yet)"),
        };
        Ok(Output::ok(
            json!({"platform": "windows", "appName": name, "appBundleId": id, "pid": found.as_ref().map(|w| w.pid), "windowTitle": found.as_ref().map(|w| w.title.clone()), "surface": "app", "message": text}),
            text,
        ))
    }

    fn close(&mut self, sid: &str, app: Option<&str>) -> Result<Output, Output> {
        let own = std::process::id();
        let all: Vec<TopWindow> = uia::top_windows().into_iter().filter(|w| w.pid != own).collect();
        let session_app = self.session(sid).app.clone();
        let (label, wins) = match (app, session_app) {
            (Some(name), _) => {
                let mut t = AppTarget { name: name.to_owned(), id: String::new(), pid: None };
                (name.to_owned(), windows_of_app(&all, &mut t))
            }
            (None, Some(mut a)) => (a.name.clone(), windows_of_app(&all, &mut a)),
            (None, None) => {
                let s = self.session(sid);
                s.last = None;
                return Ok(Output::ok(json!({"session": sid, "message": "Closed: session"}), "Closed: session".to_owned()));
            }
        };
        let closed = wins.iter().filter(|w| shell::close_window(w.hwnd)).count();
        let s = self.session(sid);
        if app.is_none() || s.app.as_ref().is_some_and(|a| a.name.eq_ignore_ascii_case(&label)) {
            s.app = None;
            s.surface = Surface::FrontmostApp;
        }
        s.last = None;
        if closed == 0 && app.is_some() {
            return Err(Output::fail("app_not_running", format!("{label} has no open windows to close.")));
        }
        let text = format!("Closed: {label}");
        Ok(Output::ok(json!({"session": sid, "appName": label, "windowsClosed": closed, "message": text}), text))
    }
}

/// Windows that belong to an app: by its process when known, else by title or executable name.
/// Records the process it found.
fn windows_of_app(all: &[TopWindow], app: &mut AppTarget) -> Vec<TopWindow> {
    if let Some(pid) = app.pid {
        let by_pid: Vec<TopWindow> = all.iter().filter(|w| w.pid == pid).cloned().collect();
        if !by_pid.is_empty() {
            return by_pid;
        }
    }
    let stem = exe_stem(&app.id);
    let name = app.name.clone();
    let hit = all.iter().find(|w| {
        contains_ci(&w.title, &name)
            || uia::process_stem(w.pid).is_some_and(|p| p.eq_ignore_ascii_case(&name) || (!stem.is_empty() && p.eq_ignore_ascii_case(&stem)))
    });
    match hit {
        Some(w) => {
            app.pid = Some(w.pid);
            all.iter().filter(|x| x.pid == w.pid).cloned().collect()
        }
        None => vec![],
    }
}
