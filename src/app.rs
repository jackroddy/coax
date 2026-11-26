use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{self, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use agent_client_protocol::Error;
use agent_client_protocol::v1::schema::SessionConfigOptionValue;
use agent_client_protocol::v1::schema::{
    ElicitationAction, InitializeResponse, PermissionOption, PermissionOptionKind,
    RequestPermissionOutcome, SelectedPermissionOutcome, SessionId, ToolCallId, ToolCallUpdate,
};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as TermEvent, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use decade_channel::Message;
use ratatui::layout::{Position, Rect};
use serde_json::json;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::sync::oneshot;

use crate::acp::{Applied, Change, Event, Link};
use crate::clipboard;
use crate::clock::Time;
use crate::command::{self, Candidate, Kind};
use crate::diff;
use crate::editor;
use crate::form::Form;
use crate::glyphs;
use crate::links::{self, Click, LinksConfig, Loaded};
use crate::log;
use crate::playback::Player;
use crate::prompt;
use crate::registry;
use crate::report::{self, Reporter, Snapshot};
use crate::select::{self, Point, Selection};
use crate::session::{self, Entry, Marked, Transcript};
use crate::settings::{Category, Control, Setting, Settings};
use crate::template::Template;
use crate::ui::{self, Word};

const WHEEL_LINES: usize = 3;

/// How long a first ^c, ^d or esc waits for the second.
pub const GUARD_WINDOW: Duration = Duration::from_millis(1500);

/// The key that armed the guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardKey {
    CtrlC,
    CtrlD,
    Esc,
}

/// A first guarded key, waiting for a second until `until`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Guard {
    pub key: GuardKey,
    pub until: Instant,
}
/// How often the working spinner steps.
const SPIN_INTERVAL: Duration = Duration::from_millis(150);

const PAN_COLUMNS: isize = 4;

/// How often a drag held above or below the transcript scrolls it a line.
const EDGE_SCROLL_INTERVAL: Duration = Duration::from_millis(50);

/// A left press on the transcript, which its release makes a click unless
/// a drag made it a selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Press {
    at: Position,

    /// Where a selection from the press starts, when the press is on text.
    anchor: Option<Point>,

    /// 1 for a click, 2 for the second of a double click, 3 for a triple.
    count: u8,
}

/// The longest wait between the clicks of a double or triple click.
const MULTI_CLICK: Duration = Duration::from_millis(400);

/// How long a block's copy glyph says `copied`.
const COPIED_FOR: Duration = Duration::from_secs(1);

/// A scroll bar thumb being dragged by the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drag {
    pub key: (usize, usize),

    /// How far into the thumb the pointer took hold.
    grab: isize,

    /// The bar's first column, its width and how far its block pans.
    bar_x: u16,
    visible: usize,
    max: usize,
}

impl Drag {
    /// The pan that keeps the thumb under the pointer at `column`.
    fn pan_at(&self, column: u16) -> usize {
        let start = column as isize - self.bar_x as isize - self.grab;
        ui::pan_for_thumb(self.visible, self.max, start, 2)
    }
}

/// A passage of a reply marked for the prompt, with where it lies in the
/// reply's source.
pub struct Mark {
    pub number: usize,
    pub text: String,
    pub entry: usize,

    /// The bytes of the reply's text the passage covers, empty when the
    /// selection held no text with a known source.
    pub source: Range<usize>,
}

/// A prompt entered during a turn, waiting for its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queued {
    pub text: String,
    pub marks: Vec<Marked>,
}

pub struct App {
    pub transcript: Transcript,
    pub input: String,

    /// The cursor's byte index into `input`, always on a char boundary.
    pub cursor: usize,

    /// The prompts sent in this run, oldest first, each with its send time.
    pub history: Vec<(String, Option<Time>)>,
    pub scroll: usize,
    pub show_plan: bool,

    /// The highlighted entry of the command list under the prompt.
    pub command_cursor: usize,

    /// Whether Esc has closed the command list for the text typed so far.
    pub command_closed: bool,

    /// Whether Up or Down has moved through the command list since the last
    /// edit, so that Enter runs the highlighted entry.
    command_picked: bool,

    /// The text typed before Tab began filling in matches, and the match Tab
    /// filled in last.
    command_cycle: Option<(String, usize)>,

    /// The prompt templates from `config.toml`, by name.
    pub templates: BTreeMap<String, Template>,

    /// The form of the template being filled in.
    pub template_form: Option<TemplateForm>,

    /// Where the template form's preview button was drawn.
    pub preview_button: Option<Rect>,

    /// An agent command waiting for a yes before it is sent.
    pub confirm: Option<Confirm>,

    /// The subagent calls shown unfolded.
    pub expanded: HashSet<ToolCallId>,

    /// The subagent blocks on screen, set by the last draw.
    pub folds: Vec<ui::FoldArea>,
    pub settings: Settings,

    /// The header chips on screen, set by the last draw.
    pub chips: Vec<ChipArea>,
    pub hover: Option<ChipTarget>,

    /// The working spinner's frame, stepped while the agent works.
    pub spinner: usize,
    pub menu: Option<Menu>,

    /// Where the open menu was drawn and the first of its rows shown, set by
    /// the last draw.
    pub menu_area: Option<Rect>,
    pub menu_first: usize,
    pub guard: Option<Guard>,

    /// How far each wide block is panned, by entry and block.
    pub pans: HashMap<(usize, usize), usize>,

    /// The `markdown` blocks shown as their source, by entry and block.
    pub raw: HashSet<(usize, usize)>,

    /// The code blocks set to wrap or to pan against their default, by
    /// entry and block.
    pub wraps: HashMap<(usize, usize), bool>,

    /// The tab each tabbed box shows, by entry and the box's first block.
    pub tabs: HashMap<(usize, usize), usize>,

    /// The toggle words on screen, set by the last draw.
    pub toggles: Vec<ui::ToggleArea>,

    /// A line shown on the prompt's rule until the next key: what the
    /// prompt's text names that is not a command, or the answer to `/log`.
    pub unknown: Option<String>,

    /// The conversation's scroll bar, set by the last draw when it shows.
    pub v_bar: Option<ui::VBar>,

    /// Whether the pointer is over the conversation's thumb.
    pub hover_v_bar: bool,

    /// How far into the conversation's thumb the pointer took hold, while
    /// it drags.
    pub v_drag: Option<isize>,

    /// The wide blocks on screen, set by the last draw.
    pub wide: Vec<ui::WideBlock>,

    /// The thumb the pointer is dragging, from a press until the release.
    pub drag: Option<Drag>,

    /// The scroll bars under wide blocks on screen, set by the last draw.
    pub bars: Vec<ui::ScrollBar>,

    /// The block whose thumb the pointer is over.
    pub hover_bar: Option<(usize, usize)>,

    /// The transcript's text, set by the last draw.
    pub texts: select::Texts,

    /// The range a drag over the transcript marked, kept until the next
    /// click.
    pub selection: Option<Selection>,

    /// The passages the prompt's chips stand for, in the order they were
    /// made.
    pub marks: Vec<Mark>,

    /// Where the last draw put the button that marks the selection.
    pub mark_button: Option<Rect>,

    /// Whether the pointer is over the mark button.
    pub hover_mark: bool,

    /// A press on the transcript, until its release.
    pub press: Option<Press>,

    /// When and where the last press on the transcript was, and its count.
    pub last_press: Option<(Instant, Position, u8)>,

    /// The copy glyphs on screen, set by the last draw.
    pub copies: Vec<ui::CopyArea>,

    /// The expandable call whose head the pointer is over.
    pub hover_fold: Option<ToolCallId>,

    /// The block whose copy glyph the pointer is over.
    pub hover_copy: Option<(usize, usize)>,

    /// The block just copied and when its glyph stops saying so.
    pub copied: Option<((usize, usize), Instant)>,

    /// The links on screen, set by the last draw.
    pub link_areas: Vec<ui::LinkArea>,

    /// The link the pointer is over.
    pub hover_link: Option<HoverLink>,

    /// What the hover commands gave, by link target.
    pub link_cache: HashMap<String, Loaded>,
    pub links: LinksConfig,

    /// The registry's version of the agent, when it is later than the
    /// installed one.
    pub update: Option<String>,

    /// Rows a page scroll moves, set by the last draw.
    pub page: usize,

    /// Columns of the prompt's text at the last draw.
    pub prompt_width: usize,

    /// Transcript lines at the last draw.
    pub lines_seen: usize,
    pub agent: String,
    pub session: SessionId,
    pub decade_pane: Option<String>,
    pub permission: Option<Permission>,

    /// The form the agent asked for, and the forms that came while it was
    /// open, oldest first.
    pub form: Option<Asked>,
    pub forms: VecDeque<Asked>,

    /// The requests that came while `permission` was open, oldest first.
    pub queued: VecDeque<Permission>,

    /// Whether the agent takes a prompt during a turn and runs it once the
    /// turn ends.
    pub queueing: bool,

    /// Prompts entered during a turn, oldest first, each waiting for the
    /// turn before it to end.
    pub waiting: VecDeque<Queued>,

    /// A held prompt whose turn has come, for the run loop to send.
    outbox: Option<String>,

    /// Replies still to come for waiting prompts a cancel dropped.
    stale_replies: usize,

    /// Replies still to come for held turns that a prompt sent while they
    /// were held has already ended in the transcript.
    held_replies: usize,
    pub rendered: ui::Rendered,
    pub cwd: PathBuf,

    auto_allow: bool,

    /// Whether the agent's connection has ended.
    pub exited: bool,
}

pub struct Permission {
    pub title: String,

    /// The lines the request's diffs add and remove.
    pub changed: (usize, usize),
    pub options: Vec<PermissionOption>,
    pub update: Box<ToolCallUpdate>,
    reply: oneshot::Sender<RequestPermissionOutcome>,
}

/// A template's form, open over the prompt.
pub struct TemplateForm {
    /// The template's name in `templates`.
    pub name: String,
    pub form: Form,

    /// Whether the filled text shows under the fields.
    pub preview: bool,
}

/// A form the agent asked for, with the channel its answer goes back on.
pub struct Asked {
    pub form: Form,
    reply: oneshot::Sender<ElicitationAction>,
}

/// The link under the pointer, and where the pointer met it.
pub struct HoverLink {
    pub key: ui::LinkKey,
    pub target: String,
    pub title: Option<String>,
    pub at: Position,
}

/// What the pointer is over, so a move that changes none of it is not drawn.
#[derive(PartialEq)]
struct Hovered {
    chip: Option<ChipTarget>,
    copy: Option<(usize, usize)>,
    v_bar: bool,
    bar: Option<(usize, usize)>,
    fold: Option<ToolCallId>,
    link: Option<ui::LinkKey>,
    mark: bool,
}

/// A header chip on screen.
pub struct ChipArea {
    pub area: Rect,
    pub target: ChipTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipTarget {
    /// The setting at this index of `Settings::settings`.
    Setting(usize),

    /// The `⋯` chip, standing for the settings from this index on.
    Overflow(usize),
}

/// An agent command, as the box that asks before sending it shows it.
pub struct Confirm {
    pub name: String,
    pub description: String,
    pub input: String,
}

impl Confirm {
    /// The prompt it sends: `/name` and then the input.
    pub fn text(&self) -> String {
        if self.input.is_empty() {
            format!("/{}", self.name)
        } else {
            format!("/{} {}", self.name, self.input)
        }
    }
}

/// The picker box open under a chip.
pub struct Menu {
    pub target: MenuTarget,

    /// The highlighted row.
    pub cursor: usize,

    /// The column of the chip the box hangs from.
    pub anchor: u16,
    pub error: Option<String>,

    /// The value of a model choice waiting for a second choice to confirm it.
    pub confirm: Option<String>,
}

pub enum MenuTarget {
    /// The setting with this id.
    Setting(String),

    /// The settings from this index on, which did not fit in the header.
    Overflow(usize),

    /// The prompts sent earlier, newest first.
    History,

    /// The prompt templates, by name.
    Templates,
}

pub enum Action {
    None,

    /// Put this text on the system clipboard.
    Copy(String),
    Change(Change),

    /// Open this text in the user's editor, then hand the result to
    /// `App::finish_edit`.
    Edit(String),

    /// Run a link's hover command and keep its output under `target`.
    Load {
        target: String,
        argv: Vec<String>,
    },

    /// Send decade's `show` to the pane `to`.
    Show {
        to: String,
        show: decade_channel::Show,
    },

    /// Run this command detached.
    Run(Vec<String>),
    Prompt(String),
    Cancel,
    Quit,
}

impl App {
    pub fn event(&mut self, event: Event) {
        match event {
            Event::Update(update) => {
                self.settings.apply(&update);
                self.transcript.apply(*update);
            }
            Event::Tokens(usage) => self.settings.used(&usage),
            Event::Setting {
                id,
                result: Ok(applied),
            } => {
                match applied {
                    Applied::Options(options) => self.settings.set_options(options),
                    Applied::Mode(id) => self.settings.set_mode(&id),
                }
                if self.menu_asked(&id) {
                    self.menu = None;
                }
            }
            Event::Setting {
                id,
                result: Err(error),
            } => {
                if self.menu_asked(&id)
                    && let Some(menu) = &mut self.menu
                {
                    log::info("error", json!({"text": error.to_string()}));
                    menu.error = Some(error.to_string());
                } else {
                    self.transcript.notice(format!("setting failed: {error}"));
                }
            }
            Event::Form {
                message,
                schema,
                reply,
            } => {
                let asked = Asked {
                    form: Form::new(message, &schema),
                    reply,
                };
                if self.form.is_none() {
                    self.form = Some(asked);
                } else {
                    self.forms.push_back(asked);
                }
            }
            Event::Permission {
                update,
                options,
                reply,
            } => {
                let title = update
                    .fields
                    .title
                    .clone()
                    .unwrap_or_else(|| update.tool_call_id.to_string());
                let permission = Permission {
                    changed: diff::size(update.fields.content.as_deref().unwrap_or(&[])),
                    title,
                    options,
                    update,
                    reply,
                };
                if self.permission.is_none() {
                    self.permission = Some(permission);
                } else {
                    self.queued.push_back(permission);
                }
                while self.auto_allow && self.permission.is_some() {
                    let allow = self.permission.as_ref().and_then(|p| {
                        p.options
                            .iter()
                            .position(|o| o.kind == PermissionOptionKind::AllowOnce)
                            .or((!p.options.is_empty()).then_some(0))
                    });
                    self.decide(allow);
                }
            }
            Event::Done(_) if self.stale_replies > 0 => self.stale_replies -= 1,
            Event::Done(done) if self.held_replies > 0 => {
                self.held_replies -= 1;
                if let Err(error) = done {
                    self.transcript.notice(format!("prompt failed: {error}"));
                }
            }
            Event::Done(Ok(stop)) => {
                self.close_permissions();
                self.transcript.finished(stop);
                self.start_waiting();
            }
            Event::Done(Err(error)) => {
                self.close_permissions();
                self.transcript.in_flight = false;
                self.transcript.cancel_running();
                self.transcript.notice(format!("prompt failed: {error}"));
                self.start_waiting();
            }
            Event::Exited(reason) => {
                self.exited = true;
                self.permission = None;
                self.queued.clear();
                self.menu = None;
                self.confirm = None;
                self.stale_replies = 0;
                self.held_replies = 0;
                self.close_permissions();
                self.return_waiting();
                self.transcript.agent_exited(&reason);
            }
        }
    }

    /// Start the turn of the oldest waiting prompt. An agent that does not
    /// queue has not been sent it yet, so it goes to `outbox`.
    fn start_waiting(&mut self) {
        let Some(next) = self.waiting.pop_front() else {
            return;
        };
        if !self.queueing {
            self.outbox = Some(session::sent_text(&next.text, &next.marks));
        }
        self.transcript.prompt_marked(next.text, next.marks);
    }

    /// Cancel the running turn, and put the waiting prompts back in the
    /// prompt box ahead of what is typed there.
    fn cancel(&mut self) -> Action {
        while self.form.is_some() {
            self.answer(ElicitationAction::Cancel);
        }
        if self.queueing {
            self.stale_replies += self.waiting.len();
        }
        self.return_waiting();
        Action::Cancel
    }

    fn return_waiting(&mut self) {
        if self.waiting.is_empty() {
            return;
        }
        let mut texts: Vec<String> = self
            .waiting
            .drain(..)
            .map(|queued| session::sent_text(&queued.text, &queued.marks))
            .collect();
        if !self.input.is_empty() {
            texts.push(std::mem::take(&mut self.input));
        }
        self.set_input(texts.join("\n\n"));
    }

    /// The held prompt whose turn has come, to send now.
    pub fn take_outbox(&mut self) -> Option<String> {
        self.outbox.take()
    }

    /// Whether the open menu is the one a change to setting `id` came from.
    fn menu_asked(&self, id: &str) -> bool {
        let Some(menu) = &self.menu else {
            return false;
        };
        match &menu.target {
            MenuTarget::Setting(asked) => asked == id,
            MenuTarget::Overflow(first) => self
                .settings
                .settings()
                .iter()
                .skip(*first)
                .any(|setting| setting.id == id),
            MenuTarget::History | MenuTarget::Templates => false,
        }
    }

    pub fn decade(&mut self, message: Message) {
        log::info("decade.recv", json!({"message": format!("{message:?}")}));
        match message {
            Message::Welcome { pane, .. } => self.decade_pane = pane.or(Some(String::new())),
            Message::Error { reason, .. } => self.transcript.notice(format!("decade: {reason}")),
            Message::Closed => {
                log::info("decade.disconnect", json!({"reason": "closed by decade"}));
                self.decade_pane = None;
                self.transcript.notice("decade went away".to_string());
            }
            Message::Panes(_)
            | Message::Screenshot { .. }
            | Message::Verb { .. }
            | Message::Show { .. }
            | Message::Highlight { .. }
            | Message::Clear { .. } => {}
        }
    }

    pub fn decide(&mut self, option: Option<usize>) {
        let Some(permission) = self.permission.take() else {
            return;
        };
        let (outcome, choice) = match option.and_then(|i| permission.options.get(i)) {
            Some(option) => (
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                    option.option_id.clone(),
                )),
                option.name.clone(),
            ),
            None => (RequestPermissionOutcome::Cancelled, "cancelled".to_string()),
        };
        let _ = permission.reply.send(outcome);
        self.transcript.permission(*permission.update, choice);
        self.permission = self.queued.pop_front();
    }

    /// Cancel the open request and the queued ones, when their turn is over.
    fn close_permissions(&mut self) {
        while self.permission.is_some() {
            self.decide(None);
        }
        while self.form.is_some() {
            self.answer(ElicitationAction::Cancel);
        }
    }

    /// Send `action` for the open form, and open the next one waiting.
    fn answer(&mut self, action: ElicitationAction) {
        if let Some(asked) = self.form.take() {
            let _ = asked.reply.send(action);
        }
        self.form = self.forms.pop_front();
    }

    /// Handle a mouse event, and say whether the screen needs a redraw. A
    /// move that changes nothing under the pointer does not.
    pub fn pointer(&mut self, mouse: MouseEvent) -> (Action, bool) {
        let before = self.hovered();
        let action = self.mouse(mouse);
        let redraw = mouse.kind != MouseEventKind::Moved || self.hovered() != before;
        (action, redraw)
    }

    /// What the pointer is over, as a draw shows it.
    fn hovered(&self) -> Hovered {
        Hovered {
            chip: self.hover,
            copy: self.hover_copy,
            v_bar: self.hover_v_bar,
            bar: self.hover_bar,
            fold: self.hover_fold.clone(),
            link: self.hover_link.as_ref().map(|hover| hover.key),
            mark: self.hover_mark,
        }
    }

    pub fn mouse(&mut self, mouse: MouseEvent) -> Action {
        let position = Position::new(mouse.column, mouse.row);
        let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
        let pan = match mouse.kind {
            MouseEventKind::Moved => {
                self.hover = self.chip_at(position);
                self.hover_copy = self.copy_at(position);
                self.hover_mark = self
                    .mark_button
                    .is_some_and(|button| button.contains(position));
                self.hover_v_bar = self
                    .v_bar_at(position)
                    .is_some_and(|(_, grab)| grab.is_some());
                self.hover_bar = self
                    .bar_at(position)
                    .filter(|(_, grab)| grab.is_some())
                    .map(|(bar, _)| bar.key);
                self.hover_fold = self
                    .folds
                    .iter()
                    .find(|fold| fold.whole.contains(position))
                    .map(|fold| fold.id.clone());
                return self.hover_over(position);
            }
            MouseEventKind::Down(MouseButton::Left) => return self.click(position),
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(press) = self.press
                    && let Some(anchor) = press.anchor
                    && position != press.at
                {
                    let head = self.texts.nearest(position);
                    self.last_press = None;
                    let selection = self.selection.get_or_insert(Selection {
                        anchor,
                        head,
                        pointer: None,
                    });
                    selection.head = head;
                    selection.pointer = Some(position);
                }
                if let Some(drag) = self.drag {
                    self.pans.insert(drag.key, drag.pan_at(position.x));
                }
                if let (Some(grab), Some(bar)) = (self.v_drag, &self.v_bar) {
                    let start = position.y as isize - bar.area.y as isize - grab;
                    self.scroll = bar.scroll_for_thumb(start);
                }
                return Action::None;
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.drag = None;
                self.v_drag = None;
                return self.release();
            }
            MouseEventKind::ScrollLeft => -PAN_COLUMNS,
            MouseEventKind::ScrollUp if shift => -PAN_COLUMNS,
            MouseEventKind::ScrollRight => PAN_COLUMNS,
            MouseEventKind::ScrollDown if shift => PAN_COLUMNS,
            MouseEventKind::ScrollUp => {
                self.scroll += WHEEL_LINES;
                return Action::None;
            }
            MouseEventKind::ScrollDown => {
                self.scroll = self.scroll.saturating_sub(WHEEL_LINES);
                return Action::None;
            }
            _ => return Action::None,
        };
        if let Some(block) = self.wide.iter().find(|block| block.area.contains(position)) {
            let offset = self.pans.get(&block.key).copied().unwrap_or(0) as isize;
            let offset = (offset + pan).clamp(0, block.max as isize) as usize;
            self.pans.insert(block.key, offset);
        }
        Action::None
    }

    /// End a press on the transcript: copy what its drag selected, or else
    /// act on a click at the press.
    fn release(&mut self) -> Action {
        let Some(press) = self.press.take() else {
            return Action::None;
        };
        if let Some(selection) = &mut self.selection {
            selection.pointer = None;
            let text = selection.copy(&self.texts.lines);
            if text.is_empty() {
                return Action::None;
            }
            return Action::Copy(text);
        }
        let picked = press.anchor.and_then(|point| match press.count {
            2 => select::word_at(&self.texts.lines, point),
            3 => select::line_at(&self.texts.lines, point),
            _ => None,
        });
        if let Some(selection) = picked {
            let text = selection.copy(&self.texts.lines);
            self.selection = Some(selection);
            return Action::Copy(text);
        }
        if let Some(action) = self.click_link(press.at) {
            return action;
        }
        if let Some(fold) = self.folds.iter().find(|fold| fold.whole.contains(press.at)) {
            self.flip_fold(fold.id.clone());
        }
        Action::None
    }

    /// Which way a drag held past the transcript's top or bottom scrolls
    /// it: 1 up, -1 down, 0 when the pointer is not past either.
    pub fn edge_scroll(&self) -> isize {
        let Some(pointer) = self.selection.as_ref().and_then(|s| s.pointer) else {
            return 0;
        };
        let area = self.texts.area;
        if pointer.y < area.y {
            1
        } else if pointer.y >= area.bottom() && self.scroll > 0 {
            -1
        } else {
            0
        }
    }

    /// Scroll a line the way `edge_scroll` says.
    pub fn scroll_past_edge(&mut self) {
        self.scroll = self.scroll.saturating_add_signed(self.edge_scroll());
    }

    /// Move the hover to the link at `position`, and ask for its hover
    /// command to run when it has not been.
    fn hover_over(&mut self, position: Position) -> Action {
        let covered = self.permission.is_some() || self.confirm.is_some() || self.menu.is_some();
        let area = self
            .link_areas
            .iter()
            .find(|link| link.area.contains(position))
            .filter(|_| !covered);
        let Some(area) = area else {
            self.hover_link = None;
            return Action::None;
        };
        if self.hover_link.as_ref().map(|hover| hover.key) == Some(area.key) {
            return Action::None;
        }
        let target = area.target.clone();
        self.hover_link = Some(HoverLink {
            key: area.key,
            target: target.clone(),
            title: area.title.clone(),
            at: position,
        });
        if self.link_cache.contains_key(&target) {
            return Action::None;
        }
        match links::hover_command(&target, &self.links) {
            Some(argv) => {
                self.link_cache.insert(target.clone(), Loaded::Loading);
                Action::Load { target, argv }
            }
            None => Action::None,
        }
    }

    /// Keep what a hover command gave.
    pub fn loaded(&mut self, target: String, result: Result<String, String>) {
        let loaded = match result {
            Ok(text) => Loaded::Text(text),
            Err(error) => Loaded::Failed(error),
        };
        self.link_cache.insert(target, loaded);
    }

    /// What a click on the link at `position` does, if it is on one.
    fn click_link(&mut self, position: Position) -> Option<Action> {
        let area = self
            .link_areas
            .iter()
            .find(|link| link.area.contains(position))?;
        let action = match links::click(&area.target, &self.links, &self.cwd)? {
            Click::Run(argv) => Action::Run(argv),
            Click::Show { .. } if self.decade_pane.is_none() => {
                self.unknown = Some("no decade connection".to_string());
                Action::None
            }
            Click::Show { to, show } => Action::Show { to, show },
        };
        Some(action)
    }

    /// The scroll bar under `position`, and how far into its thumb that is
    /// when it is on the thumb.
    fn bar_at(&self, position: Position) -> Option<(&ui::ScrollBar, Option<isize>)> {
        let bar = self.bars.iter().find(|bar| bar.area.contains(position))?;
        let offset = self.pans.get(&bar.key).copied().unwrap_or(0);
        let (start, len) = ui::thumb(bar.area.width as usize, bar.max, offset, 2);
        let column = (position.x - bar.area.x) as usize;
        let on_thumb = (start..start + len).contains(&column);
        Some((bar, on_thumb.then(|| (column - start) as isize)))
    }

    /// The conversation's scroll bar under `position`, and how far into its
    /// thumb that is when it is on the thumb.
    fn v_bar_at(&self, position: Position) -> Option<(&ui::VBar, Option<isize>)> {
        let bar = self
            .v_bar
            .as_ref()
            .filter(|bar| bar.area.contains(position))?;
        let (start, len) = bar.thumb(self.scroll);
        let row = (position.y - bar.area.y) as usize;
        let on_thumb = (start..start + len).contains(&row);
        Some((bar, on_thumb.then(|| (row - start) as isize)))
    }

    /// Take hold of the conversation's thumb under a press, or move it to
    /// the press first when the press is on the track.
    fn press_v_bar(&mut self, position: Position) -> bool {
        let Some((bar, grab)) = self.v_bar_at(position) else {
            return false;
        };
        let grab = match grab {
            Some(grab) => grab,
            None => {
                let len = bar.thumb(0).1 as isize;
                let start = position.y as isize - bar.area.y as isize - len / 2;
                self.scroll = bar.scroll_for_thumb(start);
                len / 2
            }
        };
        self.v_drag = Some(grab);
        true
    }

    /// Take hold of the thumb under a press, or move it to the press first
    /// when the press is on the track.
    fn flip_fold(&mut self, id: ToolCallId) {
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
    }

    fn press_bar(&mut self, position: Position) -> bool {
        let Some((bar, grab)) = self.bar_at(position) else {
            return false;
        };
        let len = ui::thumb(bar.area.width as usize, bar.max, 0, 2).1;
        let drag = Drag {
            key: bar.key,
            grab: grab.unwrap_or(len as isize / 2),
            bar_x: bar.area.x,
            visible: bar.area.width as usize,
            max: bar.max,
        };
        if grab.is_none() {
            self.pans.insert(drag.key, drag.pan_at(position.x));
        }
        self.drag = Some(drag);
        true
    }

    fn copy_at(&self, position: Position) -> Option<(usize, usize)> {
        self.copies
            .iter()
            .find(|copy| copy.area.contains(position))
            .map(|copy| copy.key)
    }

    fn chip_at(&self, position: Position) -> Option<ChipTarget> {
        self.chips
            .iter()
            .find(|chip| chip.area.contains(position))
            .map(|chip| chip.target)
    }

    fn click(&mut self, position: Position) -> Action {
        if let Some(open) = &mut self.template_form {
            if self
                .preview_button
                .is_some_and(|button| button.contains(position))
            {
                open.preview = !open.preview;
            }
            return Action::None;
        }
        if self
            .mark_button
            .is_some_and(|button| button.contains(position))
        {
            self.mark();
            return Action::None;
        }
        self.selection = None;
        if self.permission.is_some() || self.form.is_some() || self.confirm.is_some() {
            return Action::None;
        }
        if self.menu.is_some() {
            let Some(area) = self.menu_area.filter(|area| area.contains(position)) else {
                self.menu = None;
                return Action::None;
            };
            let row = (position.y.saturating_sub(area.y + 1)) as usize + self.menu_first;
            if position.y > area.y && row < self.menu_rows() {
                if let Some(menu) = &mut self.menu {
                    menu.cursor = row;
                }
                return self.choose();
            }
            return Action::None;
        }
        if self.press_v_bar(position) {
            return Action::None;
        }
        let Some(chip) = self.chips.iter().find(|chip| chip.area.contains(position)) else {
            if let Some(toggle) = self
                .toggles
                .iter()
                .find(|toggle| toggle.area.contains(position))
            {
                match toggle.word {
                    Word::Render => {
                        self.raw.remove(&toggle.key);
                    }
                    Word::Raw => {
                        self.raw.insert(toggle.key);
                    }
                    Word::Wrap => {
                        self.wraps.insert(toggle.key, true);
                    }
                    Word::Pan => {
                        self.wraps.insert(toggle.key, false);
                    }
                    Word::Tab(tab) | Word::More(tab) => {
                        self.tabs.insert(toggle.key, tab);
                    }
                }
                return Action::None;
            }
            if let Some(key) = self.copy_at(position) {
                let Some(text) = self.rendered.copy_text(key) else {
                    return Action::None;
                };
                let text = text.to_string();
                self.copied = Some((key, Instant::now() + COPIED_FOR));
                return Action::Copy(text);
            }
            if self.press_bar(position) {
                return Action::None;
            }

            // a drag in a wide block selects nothing, since its rows pan
            let anchor = self
                .texts
                .text_at(position)
                .filter(|_| !self.wide.iter().any(|block| block.area.contains(position)));
            let now = Instant::now();
            let count = match self.last_press {
                Some((at, place, count))
                    if place == position && now.duration_since(at) < MULTI_CLICK =>
                {
                    count % 3 + 1
                }
                _ => 1,
            };
            self.last_press = Some((now, position, count));
            self.press = Some(Press {
                at: position,
                anchor,
                count,
            });
            return Action::None;
        };
        let anchor = chip.area.x;
        match chip.target {
            ChipTarget::Setting(index) => self.activate(index, anchor),
            ChipTarget::Overflow(first) => {
                self.menu = Some(Menu {
                    target: MenuTarget::Overflow(first),
                    cursor: 0,
                    anchor,
                    error: None,
                    confirm: None,
                });
                Action::None
            }
        }
    }

    /// Whether a turn has been played, so a model change costs a reread.
    fn has_conversation(&self) -> bool {
        self.transcript
            .entries
            .iter()
            .any(|entry| matches!(entry, Entry::User(_) | Entry::Agent(_)))
    }

    /// Do what a click on the chip of setting `index` does: flip a yes or no
    /// setting, or open a choice's box under column `anchor`. The model waits
    /// while the agent works.
    fn activate(&mut self, index: usize, anchor: u16) -> Action {
        let settings = self.settings.settings();
        let Some(setting) = settings.get(index) else {
            return Action::None;
        };
        match &setting.control {
            Control::Flip(on) => Action::Change(flip(&setting.id, *on)),
            Control::Choose(_)
                if setting.category == Category::Model && self.transcript.in_flight =>
            {
                Action::None
            }
            Control::Choose(choices) => {
                self.menu = Some(Menu {
                    target: MenuTarget::Setting(setting.id.clone()),
                    cursor: choices.iter().position(|c| c.current).unwrap_or(0),
                    anchor,
                    error: None,
                    confirm: None,
                });
                Action::None
            }
        }
    }

    /// The commands the prompt's text can complete to, while a name is being
    /// typed.
    pub fn popup(&self) -> Vec<Candidate> {
        let busy = self.permission.is_some() || self.menu.is_some() || self.confirm.is_some();
        if busy || self.command_closed || !command::naming(&self.input) {
            return Vec::new();
        }
        command::matches(
            &self.input,
            &self.settings.settings(),
            &self.settings.commands,
            &self.templates,
        )
    }

    /// Enter on a command: run the command the prompt names, the entry
    /// picked from the list, or the only entry left. With several entries
    /// left and none picked, the list stays open.
    fn enter_command(&mut self) -> Action {
        let settings = self.settings.settings();
        let list = self.popup();
        let picked = self
            .command_picked
            .then(|| list.get(self.command_cursor).cloned())
            .flatten();
        let entry = match self.exact(&settings).or(picked) {
            Some(entry) => Some(entry),
            None if list.len() == 1 => list.into_iter().next(),
            None if list.len() > 1 => return Action::None,
            None => None,
        };
        let Some(entry) = entry else {
            self.unknown = command::unknown(&self.input);
            if let Some(text) = &self.unknown {
                log::info("notice", json!({"text": text}));
            }
            return Action::None;
        };
        log::info("command", json!({"name": entry.fill}));
        match entry.kind {
            Kind::Log => {
                self.clear_input();
                self.unknown = Some(match log::path() {
                    Some(path) => format!("log: {}", path.display()),
                    None => "no log file".to_string(),
                });
                Action::None
            }
            Kind::Coax(index) => {
                self.clear_input();
                let anchor = self
                    .chips
                    .iter()
                    .find(|chip| chip.target == ChipTarget::Setting(index))
                    .map_or(0, |chip| chip.area.x);
                self.activate(index, anchor)
            }
            Kind::History => {
                self.clear_input();
                if self.history.is_empty() {
                    self.transcript.notice("no earlier prompts".to_string());
                    return Action::None;
                }
                self.open_history();
                Action::None
            }
            Kind::Templates => {
                self.clear_input();
                if self.templates.is_empty() {
                    self.unknown = Some("no templates in config.toml".to_string());
                    return Action::None;
                }
                self.menu = Some(Menu {
                    target: MenuTarget::Templates,
                    cursor: 0,
                    anchor: 0,
                    error: None,
                    confirm: None,
                });
                Action::None
            }
            Kind::Template { name } => {
                self.clear_input();
                self.choose_template(&name)
            }
            Kind::Edit => {
                let text = match command::parse(&self.input) {
                    command::Typed::Coax { input, .. } => input.to_string(),
                    _ => String::new(),
                };
                Action::Edit(text)
            }
            Kind::Agent { name } => {
                if self.transcript.in_flight || self.exited {
                    return Action::None;
                }
                let input = match command::parse(&self.input) {
                    command::Typed::Agent { input, .. } => input.trim_end().to_string(),
                    _ => String::new(),
                };
                self.confirm = Some(Confirm {
                    name,
                    description: entry.description,
                    input,
                });
                Action::None
            }
        }
    }

    /// Fill the next match of what was typed into the prompt, or the one
    /// before when `forward` is false, coming round at either end.
    fn complete(&mut self, forward: bool) {
        let typed = match &self.command_cycle {
            Some((typed, _)) => typed.clone(),
            None => self.input.clone(),
        };
        let list = command::matches(
            &typed,
            &self.settings.settings(),
            &self.settings.commands,
            &self.templates,
        );
        if list.is_empty() {
            return;
        }
        let next = match self.command_cycle {
            Some((_, at)) if forward => (at + 1) % list.len(),
            Some((_, at)) => (at + list.len() - 1) % list.len(),
            None if forward => 0,
            None => list.len() - 1,
        };
        self.set_input(list[next].fill.clone());
        self.command_cursor = 0;
        self.command_picked = false;
        self.command_cycle = Some((typed, next));
    }

    /// The command named exactly by the prompt's text, if there is one.
    /// `/t ` with no name yet names the template list.
    fn exact(&self, settings: &[Setting]) -> Option<Candidate> {
        if let Some(name) = command::template_name(&self.input) {
            let all = &self.settings.commands;
            return if name.trim().is_empty() {
                command::matches("/t", settings, all, &self.templates)
                    .into_iter()
                    .find(|candidate| candidate.kind == Kind::Templates)
            } else {
                command::matches(&self.input, settings, all, &self.templates)
                    .into_iter()
                    .find(|candidate| candidate.fill == format!("/t {}", name.trim_end()))
            };
        }
        let name = match command::parse(&self.input) {
            command::Typed::Coax { name, .. } | command::Typed::Agent { name, .. } => name,
            command::Typed::Plain => return None,
        };
        command::matches(
            &self.input,
            settings,
            &self.settings.commands,
            &self.templates,
        )
        .into_iter()
        .find(|candidate| candidate.fill.trim_start_matches('/').trim_end() == name)
    }

    fn confirm_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Enter => {
                let Some(confirm) = self.confirm.take() else {
                    return Action::None;
                };
                if self.exited {
                    return Action::None;
                }
                let text = confirm.text();
                self.remember(&text, Some(Time::now()));
                self.clear_input();
                self.transcript.prompt(text.clone());
                self.scroll = 0;
                Action::Prompt(text)
            }
            KeyCode::Esc => {
                self.confirm = None;
                Action::None
            }
            _ => Action::None,
        }
    }

    fn open_history(&mut self) {
        self.menu = Some(Menu {
            target: MenuTarget::History,
            cursor: 0,
            anchor: 0,
            error: None,
            confirm: None,
        });
    }

    /// Keep the prompts of a loaded session for `/history`, oldest first.
    pub fn remember_loaded(&mut self) {
        let loaded: Vec<(String, Option<Time>)> = self
            .transcript
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::User(prompt) => Some((prompt.text.clone(), prompt.time)),
                _ => None,
            })
            .collect();
        for (text, time) in loaded {
            self.remember(&text, time);
        }
    }

    /// Keep a sent prompt for `/history`, unless it repeats the last one.
    fn remember(&mut self, text: &str, time: Option<Time>) {
        if self.history.last().is_none_or(|(last, _)| last != text) {
            self.history.push((text.to_string(), time));
        }
    }

    fn set_input(&mut self, text: String) {
        self.cursor = text.len();
        self.input = text;
    }

    fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
    }

    fn insert(&mut self, text: &str) {
        self.input.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.edited();
    }

    /// Remove the text from `from` to the cursor, or from the cursor to
    /// `from` when that is later.
    fn remove_between(&mut self, from: usize) {
        let (start, end) = (from.min(self.cursor), from.max(self.cursor));
        self.input.replace_range(start..end, "");
        self.cursor = start;
        self.edited();
    }

    /// Insert pasted text at the cursor with its newlines kept. A paste
    /// never sends, and does nothing while a box has the keys.
    pub fn paste(&mut self, text: &str) {
        if self.permission.is_some() || self.confirm.is_some() || self.menu.is_some() {
            return;
        }
        self.unknown = None;
        if let Some(open) = &mut self.template_form {
            open.form.paste(text);
            return;
        }
        self.insert(&text.replace("\r\n", "\n").replace('\r', "\n"));
    }

    /// Take the editor's result: its text replaces the prompt with the
    /// cursor at the end, and a failure leaves the prompt as it was.
    pub fn finish_edit(&mut self, result: Result<String, String>) {
        match result {
            Ok(text) => {
                self.set_input(text);
                self.edited();
            }
            Err(message) => self.transcript.notice(message),
        }
    }

    fn edited(&mut self) {
        self.command_cursor = 0;
        self.command_closed = false;
        self.command_picked = false;
        self.command_cycle = None;
        let input = &self.input;
        self.marks
            .retain(|mark| input.contains(session::chip(mark.number)));
    }

    /// Mark the selection: put its chip in the prompt at the cursor and keep
    /// its passage.
    fn mark(&mut self) {
        let Some(selection) = self.selection.take() else {
            return;
        };
        self.mark_button = None;
        let (from, _) = selection.ends();
        let Some(entry) = self.texts.entry_of(from.line) else {
            return;
        };
        let Some(number) = (1..=session::MAX_MARKS)
            .find(|number| self.marks.iter().all(|mark| mark.number != *number))
        else {
            return;
        };
        let reply = match &self.transcript.entries[entry] {
            Entry::Agent(reply) => Some(reply.text.as_str()),
            _ => None,
        };
        let passage = reply
            .zip(selection.source(&self.texts.lines))
            .and_then(|(reply, range)| {
                let range = select::widened(reply, range);
                Some((reply.get(range.clone())?.to_string(), range))
            });
        let (text, source) = passage.unwrap_or_else(|| (selection.copy(&self.texts.lines), 0..0));
        self.marks.push(Mark {
            number,
            text,
            entry,
            source,
        });

        // a space after the chip, and before it when it would touch a word,
        // since fonts draw a circled digit wider than the one column it
        // takes and it would run into the cursor or the text beside it
        let mut chip = String::new();
        if self.input[..self.cursor]
            .chars()
            .last()
            .is_some_and(|ch| !ch.is_whitespace())
        {
            chip.push(' ');
        }
        chip.push(session::chip(number));
        chip.push(' ');
        self.insert(&chip);
    }

    /// The marks whose chips the prompt being sent holds, and no more marks
    /// left over.
    fn take_marks(&mut self, text: &str) -> Vec<Marked> {
        let mut marks: Vec<Marked> = std::mem::take(&mut self.marks)
            .into_iter()
            .filter(|mark| text.contains(session::chip(mark.number)))
            .map(|mark| Marked {
                number: mark.number,
                text: mark.text,
            })
            .collect();
        marks.sort_by_key(|mark| mark.number);
        marks
    }

    /// How many rows the open menu has.
    pub fn menu_rows(&self) -> usize {
        let Some(menu) = &self.menu else {
            return 0;
        };
        let settings = self.settings.settings();
        match &menu.target {
            MenuTarget::Setting(id) => settings.iter().find(|setting| setting.id == *id).map_or(
                0,
                |setting| match &setting.control {
                    Control::Choose(choices) => choices.len(),
                    Control::Flip(_) => 0,
                },
            ),
            MenuTarget::Overflow(first) => settings.len().saturating_sub(*first),
            MenuTarget::History => self.history.len(),
            MenuTarget::Templates => self.templates.len(),
        }
    }

    fn menu_key(&mut self, key: KeyEvent) -> Action {
        let rows = self.menu_rows();
        let Some(menu) = &mut self.menu else {
            return Action::None;
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => menu.cursor = menu.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                menu.cursor = (menu.cursor + 1).min(rows.saturating_sub(1));
            }
            KeyCode::Enter => return self.choose(),
            KeyCode::Esc => self.menu = None,
            _ => {}
        }
        Action::None
    }

    /// Apply the highlighted row of the open menu.
    fn choose(&mut self) -> Action {
        let Some(menu) = &self.menu else {
            return Action::None;
        };
        let settings = self.settings.settings();
        match &menu.target {
            MenuTarget::Setting(id) => {
                let Some(setting) = settings.iter().find(|setting| setting.id == *id) else {
                    return Action::None;
                };
                let Control::Choose(choices) = &setting.control else {
                    return Action::None;
                };
                let Some(choice) = choices.get(menu.cursor) else {
                    return Action::None;
                };
                let change = if setting.from_modes {
                    Change::Mode(choice.value.clone())
                } else {
                    Change::Config {
                        id: setting.id.clone(),
                        value: SessionConfigOptionValue::value_id(choice.value.clone()),
                    }
                };
                if !(setting.category == Category::Model && self.has_conversation()) {
                    return Action::Change(change);
                }
                // a model change in a conversation waits for a second choice
                // of the same model
                if choice.current {
                    self.menu = None;
                } else if menu.confirm.as_deref() == Some(choice.value.as_str()) {
                    return Action::Change(change);
                } else if let Some(menu) = &mut self.menu {
                    menu.confirm = Some(choice.value.clone());
                }
                Action::None
            }
            MenuTarget::History => {
                let cursor = menu.cursor;
                let Some((text, _)) = self.history.iter().rev().nth(cursor).cloned() else {
                    return Action::None;
                };
                self.menu = None;
                self.set_input(text);
                self.edited();
                Action::None
            }
            MenuTarget::Templates => {
                let Some(name) = self.templates.keys().nth(menu.cursor).cloned() else {
                    return Action::None;
                };
                self.menu = None;
                self.clear_input();
                self.choose_template(&name)
            }
            MenuTarget::Overflow(first) => {
                let Some(setting) = settings.get(first + menu.cursor) else {
                    return Action::None;
                };
                match &setting.control {
                    Control::Flip(on) => Action::Change(flip(&setting.id, *on)),
                    Control::Choose(_)
                        if setting.category == Category::Model && self.transcript.in_flight =>
                    {
                        Action::None
                    }
                    Control::Choose(choices) => {
                        self.menu = Some(Menu {
                            target: MenuTarget::Setting(setting.id.clone()),
                            cursor: choices.iter().position(|c| c.current).unwrap_or(0),
                            anchor: menu.anchor,
                            error: None,
                            confirm: None,
                        });
                        Action::None
                    }
                }
            }
        }
    }

    /// Enter on plain text: send the prompt, or queue it behind a turn.
    fn send(&mut self, now: Instant) -> Action {
        if self.exited || self.input.trim().is_empty() {
            return Action::None;
        }
        let text = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.remember(&text, Some(Time::now()));
        self.scroll = 0;
        let marks = self.take_marks(&text);
        let sent = session::sent_text(&text, &marks);
        let held = self.queueing && self.waiting.is_empty() && self.transcript.held(now);
        if held {
            self.held_replies += 1;
            self.transcript.end_turn();
        } else if self.transcript.in_flight {
            self.waiting.push_back(Queued { text, marks });
            return if self.queueing {
                Action::Prompt(sent)
            } else {
                Action::None
            };
        }
        self.transcript.prompt_marked(text, marks);
        Action::Prompt(sent)
    }

    /// Open the form of the template `name`, or put its text in the prompt
    /// when it has no blanks.
    fn choose_template(&mut self, name: &str) -> Action {
        let Some(chosen) = self.templates.get(name) else {
            self.unknown = Some(format!("no template {name}"));
            return Action::None;
        };
        if chosen.fields.is_empty() {
            let text = chosen.fill(&BTreeMap::new()).unwrap_or_default();
            self.set_input(text);
            self.edited();
            return Action::None;
        }
        self.template_form = Some(TemplateForm {
            name: name.to_string(),
            form: Form::template(name, chosen),
            preview: chosen.preview,
        });
        Action::None
    }

    /// Finish the open template form: its filled text goes in the prompt.
    /// A field that needs an answer keeps the form open with the cursor on
    /// it.
    fn finish_template(&mut self) -> Action {
        let Some(open) = &mut self.template_form else {
            return Action::None;
        };
        let Some(template) = self.templates.get(&open.name) else {
            self.template_form = None;
            return Action::None;
        };
        match template.fill(&open.form.answers()) {
            Ok(text) => {
                self.template_form = None;
                self.set_input(text);
                self.edited();
                Action::None
            }
            Err((index, message)) => {
                let key = &template.fields[index].0;
                if let Some(field) = open.form.fields.iter().position(|f| &f.key == key) {
                    open.form.refuse(field, message);
                }
                Action::None
            }
        }
    }

    /// Handle a key pressed at `now`. Any key but the one that completes
    /// the guard disarms it.
    pub fn key(&mut self, key: KeyEvent, now: Instant) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        self.unknown = None;
        let armed = self.guard.take().filter(|guard| guard.until > now);
        let exit = match key.code {
            KeyCode::Char('c') if ctrl => Some(GuardKey::CtrlC),
            KeyCode::Char('d') if ctrl && self.input.is_empty() => Some(GuardKey::CtrlD),
            _ => None,
        };
        if let Some(exit) = exit {
            if armed.is_some_and(|guard| guard.key == exit) {
                return Action::Quit;
            }
            let working = self.transcript.in_flight && self.permission.is_none();
            if exit == GuardKey::CtrlC && working {
                self.guard = Some(Guard {
                    key: exit,
                    until: now + GUARD_WINDOW,
                });
                return self.cancel();
            }
            if exit == GuardKey::CtrlC && !self.input.is_empty() {
                self.clear_input();
                return Action::None;
            }
            self.guard = Some(Guard {
                key: exit,
                until: now + GUARD_WINDOW,
            });
            return Action::None;
        }
        if let Some(asked) = &mut self.form {
            if let Some(action) = asked.form.key(key) {
                self.answer(action);
            }
            return Action::None;
        }
        if let Some(open) = &mut self.template_form {
            if key.code == KeyCode::Char('p') && ctrl {
                open.preview = !open.preview;
                return Action::None;
            }
            return match open.form.key(key) {
                Some(ElicitationAction::Accept(_)) => self.finish_template(),
                Some(_) => {
                    self.template_form = None;
                    Action::None
                }
                None => Action::None,
            };
        }
        if let Some(permission) = &self.permission {
            match key.code {
                KeyCode::Esc => self.decide(None),
                KeyCode::Char(digit) if digit.is_ascii_digit() => {
                    let index = digit.to_digit(10).unwrap() as usize;
                    if (1..=permission.options.len()).contains(&index) {
                        self.decide(Some(index - 1));
                    }
                }
                _ => {}
            }
            return Action::None;
        }
        if self.confirm.is_some() {
            return self.confirm_key(key);
        }
        if self.menu.is_some() {
            return self.menu_key(key);
        }
        let list = self.popup().len();
        match key.code {
            KeyCode::Up if list > 0 => {
                self.command_cursor = self.command_cursor.saturating_sub(1);
                self.command_picked = true;
                return Action::None;
            }
            KeyCode::Down if list > 0 => {
                self.command_cursor = (self.command_cursor + 1).min(list - 1);
                self.command_picked = true;
                return Action::None;
            }
            KeyCode::Tab if list > 0 => {
                self.complete(true);
                return Action::None;
            }
            KeyCode::BackTab if list > 0 => {
                self.complete(false);
                return Action::None;
            }
            KeyCode::Esc if list > 0 => {
                self.command_closed = true;
                return Action::None;
            }
            _ => {}
        }
        match key.code {
            KeyCode::Char('t') if ctrl => {
                self.show_plan = !self.show_plan;
                Action::None
            }
            KeyCode::Char('g') if ctrl => Action::Edit(self.input.clone()),
            KeyCode::Char('u') if ctrl => {
                self.clear_input();
                self.edited();
                Action::None
            }
            KeyCode::Esc if self.transcript.in_flight => {
                if armed.is_some_and(|guard| guard.key == GuardKey::Esc) {
                    return self.cancel();
                }
                self.guard = Some(Guard {
                    key: GuardKey::Esc,
                    until: now + GUARD_WINDOW,
                });
                Action::None
            }
            KeyCode::Enter if alt || shift => {
                self.insert("\n");
                Action::None
            }
            KeyCode::Up if self.input.is_empty() => {
                if !self.history.is_empty() {
                    self.open_history();
                }
                Action::None
            }
            KeyCode::Up => {
                if let Some(at) = prompt::row_above(&self.input, self.cursor, self.prompt_width) {
                    self.cursor = at;
                }
                Action::None
            }
            KeyCode::Down => {
                if let Some(at) = prompt::row_below(&self.input, self.cursor, self.prompt_width) {
                    self.cursor = at;
                }
                Action::None
            }
            KeyCode::Enter if !matches!(command::parse(&self.input), command::Typed::Plain) => {
                self.enter_command()
            }
            KeyCode::Enter => self.send(now),
            KeyCode::Backspace => {
                self.remove_between(prompt::char_before(&self.input, self.cursor));
                Action::None
            }
            KeyCode::Delete => {
                self.remove_between(prompt::char_after(&self.input, self.cursor));
                Action::None
            }
            KeyCode::Char('w') if ctrl => {
                self.remove_between(prompt::word_before(&self.input, self.cursor));
                Action::None
            }
            KeyCode::Char('k') if ctrl => {
                let mut end = prompt::line_end(&self.input, self.cursor);
                if end == self.cursor {
                    end = prompt::char_after(&self.input, end);
                }
                self.remove_between(end);
                Action::None
            }
            KeyCode::Left if alt || ctrl => {
                self.cursor = prompt::word_before(&self.input, self.cursor);
                Action::None
            }
            KeyCode::Char('b') if alt => {
                self.cursor = prompt::word_before(&self.input, self.cursor);
                Action::None
            }
            KeyCode::Right if alt || ctrl => {
                self.cursor = prompt::word_after(&self.input, self.cursor);
                Action::None
            }
            KeyCode::Char('f') if alt => {
                self.cursor = prompt::word_after(&self.input, self.cursor);
                Action::None
            }
            KeyCode::Left => {
                self.cursor = prompt::char_before(&self.input, self.cursor);
                Action::None
            }
            KeyCode::Right => {
                self.cursor = prompt::char_after(&self.input, self.cursor);
                Action::None
            }
            KeyCode::Char('a') if ctrl => {
                self.cursor = prompt::line_start(&self.input, self.cursor);
                Action::None
            }
            KeyCode::Char('e') if ctrl => {
                self.cursor = prompt::line_end(&self.input, self.cursor);
                Action::None
            }
            KeyCode::Home => {
                self.cursor = 0;
                Action::None
            }
            KeyCode::PageUp => {
                self.scroll += self.page;
                Action::None
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(self.page);
                Action::None
            }
            KeyCode::End if self.input.is_empty() => {
                self.scroll = 0;
                Action::None
            }
            KeyCode::End => {
                self.cursor = self.input.len();
                Action::None
            }
            KeyCode::Char(ch) if !ctrl => {
                self.insert(ch.encode_utf8(&mut [0; 4]));
                Action::None
            }
            _ => Action::None,
        }
    }
}

/// The terminal's events, read on a thread of their own.
pub type Keys = UnboundedReceiver<io::Result<TermEvent>>;

/// Which session a run opens on.
pub enum Start {
    New,

    /// The session, and its title as the picker listed it.
    Load(SessionId, Option<String>),
}

/// What the user asked for at launch.
pub struct Opening {
    pub start: Start,
    pub first_prompt: Option<String>,
    pub auto_allow: bool,
    pub keys: Keys,

    /// Keys a scenario types in, after the person's own.
    pub player: Player,
    pub links: LinksConfig,
    pub templates: BTreeMap<String, Template>,

    /// The agent's id in the ACP registry, for the new-version check.
    pub registry: Option<String>,

    /// The launch arguments the report to decade carries.
    pub resume_args: Vec<String>,
}

fn flip(id: &str, on: bool) -> Change {
    Change::Config {
        id: id.to_string(),
        value: SessionConfigOptionValue::boolean(!on),
    }
}

pub async fn run(
    link: Link,
    init: InitializeResponse,
    name: Option<String>,
    opening: Opening,
) -> Result<(), Error> {
    let mut terminal = ratatui::init();
    enter_input_modes();
    let restore_terminal = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        leave_input_modes();
        restore_terminal(info);
    }));
    let result = session(&mut terminal, link, init, name, opening).await;
    restore();
    result
}

/// Open the session on the terminal `run` set up, and drive it.
async fn session(
    terminal: &mut ratatui::DefaultTerminal,
    mut link: Link,
    init: InitializeResponse,
    name: Option<String>,
    opening: Opening,
) -> Result<(), Error> {
    let Opening {
        start,
        first_prompt,
        auto_allow,
        mut keys,
        mut player,
        links,
        templates,
        registry: registry_id,
        resume_args,
    } = opening;
    let version = init
        .agent_info
        .as_ref()
        .map(|info| info.version.clone())
        .unwrap_or_default();
    let queueing = init
        .agent_capabilities
        .meta
        .as_ref()
        .and_then(|meta| meta.get("claudeCode")?.get("promptQueueing")?.as_bool())
        .unwrap_or(false);
    let agent = name
        .clone()
        .or(init.agent_info.map(|info| info.name))
        .unwrap_or_else(|| "agent".to_string());
    let cwd = std::env::current_dir().map_err(Error::into_internal_error)?;
    let mut settings = Settings::default();
    let waiting = match &start {
        Start::New => format!("starting {agent}…"),
        Start::Load(..) => "loading session…".to_string(),
    };
    let kind = match &start {
        Start::New => "new",
        Start::Load(..) => "load",
    };
    if let Start::Load(_, title) = &start {
        settings.title = title.clone();
    }
    let opening_began = Instant::now();
    let session = {
        let opened = open_session(&link, start, &cwd, &mut settings);
        tokio::pin!(opened);
        loop {
            terminal
                .draw(|frame| {
                    ui::draw_waiting(frame, &waiting);
                    glyphs::fold(frame.buffer_mut());
                })
                .map_err(Error::into_internal_error)?;
            tokio::select! {
                session = &mut opened => break session?,
                key = keys.recv() => match key {
                    Some(Ok(TermEvent::Key(key)))
                        if key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        return Ok(());
                    }
                    Some(Err(error)) => return Err(Error::into_internal_error(error)),
                    None => return Ok(()),
                    Some(Ok(_)) => {}
                },
            }
        }
    };

    log::info(
        "session.open",
        json!({
            "kind": kind,
            "session": session.0,
            "ms": opening_began.elapsed().as_millis() as u64,
        }),
    );
    let restore = report::restore_argv(name.as_deref(), &resume_args, &session.0);
    let identity = decade_channel::Agent {
        name: agent.clone(),
        version,
        session: session.0.to_string(),
        args: resume_args,
    };
    let (mut reporter, mut decade_messages) = match report::connect_env(&identity, restore) {
        Some((reporter, messages)) => (Some(reporter), Some(messages)),
        None => (None, None),
    };
    let mut app = App {
        transcript: Transcript::default(),
        input: String::new(),
        cursor: 0,
        history: Vec::new(),
        scroll: 0,
        show_plan: false,
        command_cursor: 0,
        command_closed: false,
        command_picked: false,
        command_cycle: None,
        templates,
        template_form: None,
        preview_button: None,
        confirm: None,
        expanded: HashSet::new(),
        folds: Vec::new(),
        settings,
        chips: Vec::new(),
        hover: None,
        spinner: 0,
        menu: None,
        menu_area: None,
        menu_first: 0,
        guard: None,
        pans: HashMap::new(),
        raw: HashSet::new(),
        wraps: HashMap::new(),
        tabs: HashMap::new(),
        toggles: Vec::new(),
        wide: Vec::new(),
        unknown: None,
        v_bar: None,
        hover_v_bar: false,
        v_drag: None,
        drag: None,
        bars: Vec::new(),
        hover_bar: None,
        texts: select::Texts::default(),
        selection: None,
        marks: Vec::new(),
        mark_button: None,
        hover_mark: false,
        press: None,
        last_press: None,
        copies: Vec::new(),
        hover_copy: None,
        hover_fold: None,
        copied: None,
        link_areas: Vec::new(),
        hover_link: None,
        link_cache: HashMap::new(),
        links,
        update: None,
        page: 1,
        prompt_width: 80,
        lines_seen: 0,
        agent,
        session,
        decade_pane: reporter.as_ref().map(|_| String::new()),
        permission: None,
        form: None,
        forms: VecDeque::new(),
        queued: VecDeque::new(),
        queueing,
        waiting: VecDeque::new(),
        outbox: None,
        stale_replies: 0,
        held_replies: 0,
        rendered: ui::Rendered::default(),
        cwd,
        auto_allow,
        exited: false,
    };
    if let Some(problem) = log::problem() {
        app.transcript.notice(problem.to_string());
    }
    // a loaded session's history arrives before its load response
    while let Ok(event) = link.events.try_recv() {
        app.event(event);
    }
    app.transcript.end_replay();
    app.remember_loaded();
    if let Some(text) = first_prompt {
        app.transcript.prompt(text.clone());
        link.prompt(app.session.clone(), text)?;
    }
    let (loaded_tx, mut loaded) = mpsc::unbounded_channel::<(String, Result<String, String>)>();
    let (update_tx, mut updates) = mpsc::unbounded_channel::<String>();
    let installed = identity.version.clone();
    if let Some(id) = registry_id {
        tokio::spawn(async move {
            if let Some(newer) = registry::newer_than(&id, &installed).await {
                let _ = update_tx.send(newer);
            }
        });
    }
    let mut spin = tokio::time::interval(SPIN_INTERVAL);
    spin.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut edge = tokio::time::interval(EDGE_SCROLL_INTERVAL);
    edge.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut redraw = true;
    let mut screen = ratatui::buffer::Buffer::default();
    let result = loop {
        if redraw {
            let drawn = terminal.draw(|frame| {
                ui::draw(frame, &mut app);
                glyphs::fold(frame.buffer_mut());
                if player.due().is_some() {
                    screen = frame.buffer_mut().clone();
                }
            });
            if let Err(error) = drawn {
                break Err(Error::into_internal_error(error));
            }
        }
        redraw = true;
        report_to_decade(&mut reporter, &mut app);
        let expiry = app.guard.map(|guard| guard.until);
        let copied_until = app.copied.map(|(_, until)| until);
        let played = player.due();
        let action = tokio::select! {
            event = link.events.recv() => match event {
                Some(event) => {
                    app.event(event);
                    app.take_outbox().map_or(Action::None, Action::Prompt)
                }
                None => break Ok(()),
            },
            key = keys.recv() => match key {
                Some(Ok(TermEvent::Key(key))) => app.key(key, Instant::now()),
                Some(Ok(TermEvent::Paste(text))) => {
                    app.paste(&text);
                    Action::None
                }
                Some(Ok(TermEvent::Mouse(mouse))) => {
                    let (action, needs_draw) = app.pointer(mouse);
                    redraw = needs_draw;
                    action
                }
                Some(Ok(_)) => Action::None,
                Some(Err(error)) => break Err(Error::into_internal_error(error)),
                None => break Ok(()),
            },
            () = async { tokio::time::sleep_until(expiry.unwrap().into()).await }, if expiry.is_some() => {
                app.guard = None;
                Action::None
            },
            () = async { tokio::time::sleep_until(played.unwrap().into()).await }, if played.is_some() => {
                let now = Instant::now();
                match player.next(now, !app.transcript.in_flight, &screen) {
                    Some(TermEvent::Key(key)) => app.key(key, now),
                    Some(TermEvent::Mouse(mouse)) => app.pointer(mouse).0,
                    _ => {
                        redraw = false;
                        Action::None
                    }
                }
            },
            _ = spin.tick(), if app.transcript.in_flight || app.guard.is_some() || app.transcript.running_agents(Instant::now()) > 0 => {
                app.spinner = app.spinner.wrapping_add(1);
                Action::None
            },
            _ = edge.tick(), if app.edge_scroll() != 0 => {
                app.scroll_past_edge();
                Action::None
            },
            () = async { tokio::time::sleep_until(copied_until.unwrap().into()).await }, if copied_until.is_some() => {
                app.copied = None;
                Action::None
            },
            Some((target, result)) = loaded.recv() => {
                app.loaded(target, result);
                Action::None
            },
            Some(newer) = updates.recv() => {
                app.update = Some(newer);
                Action::None
            },
            message = async { decade_messages.as_mut().unwrap().recv().await }, if decade_messages.is_some() => {
                match message {
                    Some(message) => app.decade(message),
                    None => decade_messages = None,
                }
                Action::None
            },
        };
        let action = match action {
            Action::Prompt(_) | Action::Cancel | Action::Change(_) if app.exited => Action::None,
            action => action,
        };
        let outcome = match action {
            Action::None => Ok(()),
            Action::Copy(text) => {
                let mut out = io::stdout();
                out.write_all(clipboard::osc52(&text).as_bytes())
                    .and_then(|()| out.flush())
                    .map_err(Error::into_internal_error)
            }
            Action::Quit => break Ok(()),
            Action::Load { target, argv } => {
                let cwd = app.cwd.clone();
                let done = loaded_tx.clone();
                tokio::spawn(async move {
                    let result = links::run(&argv, &cwd, links::HOVER_TIMEOUT).await;
                    let _ = done.send((target, result));
                });
                Ok(())
            }
            Action::Show { to, show } => {
                if let Some(reporter) = &mut reporter
                    && let Err(error) = reporter.show(&to, &show)
                {
                    app.unknown = Some(format!("decade: {error}"));
                }
                Ok(())
            }
            Action::Run(argv) => {
                links::detach(&argv, &app.cwd);
                Ok(())
            }
            Action::Prompt(text) => link.prompt(app.session.clone(), text),
            Action::Cancel => link.cancel(app.session.clone()),
            Action::Change(change) => link.change(app.session.clone(), change),
            Action::Edit(text) => {
                let editor =
                    editor::pick(std::env::var("VISUAL").ok(), std::env::var("EDITOR").ok());
                let result = with_terminal_suspended(terminal, || editor::edit(&editor, &text));
                app.finish_edit(result);
                Ok(())
            }
        };
        if let Err(error) = outcome {
            break Err(error);
        }
    };
    if let Some(reporter) = reporter {
        reporter.end();
    }
    result
}

/// Tell decade what changed about the agent. A failed send drops the channel
/// and the session carries on without it.
fn report_to_decade(reporter: &mut Option<Reporter>, app: &mut App) {
    if app.exited
        && let Some(reporter) = reporter.take()
    {
        reporter.end();
        return;
    }
    let Some(open) = reporter else {
        return;
    };
    let question = app
        .permission
        .as_ref()
        .map(|p| p.title.as_str())
        .or(app.form.as_ref().map(|asked| asked.form.message.as_str()));
    let now = Snapshot::of(&app.transcript, question, &app.settings, Instant::now());
    if let Err(error) = open.send(now) {
        log::info("decade.disconnect", json!({"reason": error.to_string()}));
        *reporter = None;
        app.decade_pane = None;
        app.transcript.notice("decade went away".to_string());
    }
}

async fn open_session(
    link: &Link,
    start: Start,
    cwd: &Path,
    settings: &mut Settings,
) -> Result<SessionId, Error> {
    match start {
        Start::New => {
            let response = link.new_session(cwd.to_path_buf()).await?;
            settings.open(response.config_options, response.modes);
            Ok(response.session_id)
        }
        Start::Load(id, _) => {
            let response = link.load_session(id.clone(), cwd.to_path_buf()).await?;
            settings.open(response.config_options, response.modes);
            Ok(id)
        }
    }
}

/// Hand the terminal to a child process: leave the alternate screen and raw
/// mode, keep the key thread from reading, run `work`, and take the terminal
/// back with a full redraw.
fn with_terminal_suspended<T>(
    terminal: &mut ratatui::DefaultTerminal,
    work: impl FnOnce() -> T,
) -> T {
    KEYS_PAUSED.store(true, Ordering::SeqCst);
    let reading = KEYS_READING.lock();
    leave_input_modes();
    ratatui::restore();
    let result = work();
    let _ = crossterm::terminal::enable_raw_mode();
    let _ = crossterm::execute!(io::stdout(), crossterm::terminal::EnterAlternateScreen);
    enter_input_modes();
    let _ = terminal.clear();
    drop(reading);
    KEYS_PAUSED.store(false, Ordering::SeqCst);
    result
}

fn enter_input_modes() {
    let _ = crossterm::execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste);
}

fn leave_input_modes() {
    let _ = crossterm::execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
}

fn restore() {
    leave_input_modes();
    ratatui::restore();
}

/// Set while a child process owns the terminal, so the key thread leaves
/// its input alone.
static KEYS_PAUSED: AtomicBool = AtomicBool::new(false);

/// Held by the key thread around each wait for input, and by the thread that
/// pauses it, so a pause starts only between two waits.
static KEYS_READING: Mutex<()> = Mutex::new(());

const KEY_POLL: Duration = Duration::from_millis(50);

pub fn key_thread() -> Keys {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        loop {
            if KEYS_PAUSED.load(Ordering::SeqCst) {
                std::thread::sleep(KEY_POLL);
                continue;
            }
            let _reading = KEYS_READING.lock();
            let event = match crossterm::event::poll(KEY_POLL) {
                Ok(true) => crossterm::event::read(),
                Ok(false) => continue,
                Err(error) => Err(error),
            };
            let failed = event.is_err();
            if tx.send(event).is_err() || failed {
                return;
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use crossterm::event::MouseEventKind;
    use ratatui::layout::Rect;

    use super::*;
    use crate::session::Message;
    use crate::ui::WideBlock;

    fn reply(text: impl Into<String>) -> Entry {
        Entry::Agent(Message::new(text.into(), None))
    }

    fn prompt(text: impl Into<String>) -> Entry {
        Entry::User(Message::new(text.into(), None))
    }

    #[test]
    fn played_input_sends_a_prompt_and_types_the_next_only_after_the_turn_ends() {
        let scenario = crate::scenario::Scenario::parse(
            r#"{"acts": [], "input": [
                {"type": "hi", "per_char_ms": 10},
                {"key": "Enter"},
                {"wait": "idle"},
                {"type": "yo", "per_char_ms": 10},
                {"key": "Enter"}
            ]}"#,
        )
        .unwrap();
        let mut player = Player::new(scenario.input).unwrap();
        let mut app = app();
        let mut now = Instant::now();
        let mut sent = Vec::new();
        let blank = ratatui::buffer::Buffer::default();
        let mut play = |app: &mut App, now: Instant, sent: &mut Vec<String>| {
            if let Some(TermEvent::Key(key)) = player.next(now, !app.transcript.in_flight, &blank)
                && let Action::Prompt(text) = app.key(key, now)
            {
                sent.push(text);
            }
        };
        for _ in 0..10 {
            now += Duration::from_millis(10);
            play(&mut app, now, &mut sent);
        }
        assert_eq!(sent, ["hi"]);
        assert!(app.transcript.in_flight);
        assert!(app.input.is_empty());
        app.transcript
            .finished(agent_client_protocol::v1::schema::StopReason::EndTurn);
        for _ in 0..10 {
            now += Duration::from_millis(60);
            play(&mut app, now, &mut sent);
        }
        assert_eq!(sent, ["hi", "yo"]);
    }

    fn app() -> App {
        App {
            transcript: Transcript::default(),
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            scroll: 0,
            show_plan: false,
            command_cursor: 0,
            command_closed: false,
            command_picked: false,
            command_cycle: None,
            templates: BTreeMap::new(),
            template_form: None,
            preview_button: None,
            confirm: None,
            expanded: HashSet::new(),
            folds: Vec::new(),
            settings: Settings::default(),
            chips: Vec::new(),
            hover: None,
            spinner: 0,
            menu: None,
            menu_area: None,
            menu_first: 0,
            guard: None,
            pans: HashMap::new(),
            raw: HashSet::new(),
            wraps: HashMap::new(),
            tabs: HashMap::new(),
            toggles: Vec::new(),
            unknown: None,
            v_bar: None,
            hover_v_bar: false,
            v_drag: None,
            drag: None,
            bars: Vec::new(),
            hover_bar: None,
            texts: select::Texts::default(),
            selection: None,
            marks: Vec::new(),
            mark_button: None,
            hover_mark: false,
            press: None,
            last_press: None,
            copies: Vec::new(),
            hover_copy: None,
            hover_fold: None,
            copied: None,
            link_areas: Vec::new(),
            hover_link: None,
            link_cache: HashMap::new(),
            links: LinksConfig::default(),
            update: None,
            wide: vec![WideBlock {
                area: Rect::new(2, 5, 20, 3),
                key: (1, 0),
                max: 6,
            }],
            page: 1,
            prompt_width: 80,
            lines_seen: 0,
            agent: String::new(),
            session: SessionId::new("s"),
            decade_pane: None,
            permission: None,
            form: None,
            forms: VecDeque::new(),
            queued: VecDeque::new(),
            queueing: false,
            waiting: VecDeque::new(),
            outbox: None,
            stale_replies: 0,
            held_replies: 0,
            rendered: ui::Rendered::default(),
            cwd: PathBuf::new(),
            auto_allow: false,
            exited: false,
        }
    }

    fn wheel(kind: MouseEventKind, modifiers: KeyModifiers, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers,
        }
    }

    #[test]
    fn a_horizontal_wheel_over_a_wide_block_pans_it_four_columns_and_stops_at_the_ends() {
        let mut app = app();
        app.mouse(wheel(MouseEventKind::ScrollRight, KeyModifiers::NONE, 3, 6));
        assert_eq!(app.pans[&(1, 0)], 4);
        app.mouse(wheel(MouseEventKind::ScrollRight, KeyModifiers::NONE, 3, 6));
        assert_eq!(app.pans[&(1, 0)], 6);
        for _ in 0..3 {
            app.mouse(wheel(MouseEventKind::ScrollLeft, KeyModifiers::NONE, 3, 6));
        }
        assert_eq!(app.pans[&(1, 0)], 0);
    }

    #[test]
    fn shift_with_the_wheel_pans_and_a_plain_wheel_scrolls_the_conversation() {
        let mut app = app();
        app.mouse(wheel(MouseEventKind::ScrollDown, KeyModifiers::SHIFT, 3, 6));
        assert_eq!(app.pans[&(1, 0)], 4);
        assert_eq!(app.scroll, 0);
        app.mouse(wheel(MouseEventKind::ScrollUp, KeyModifiers::NONE, 3, 6));
        assert_eq!(app.scroll, 3);
        assert_eq!(app.pans[&(1, 0)], 4);
    }

    #[test]
    fn panning_away_from_every_wide_block_does_nothing() {
        let mut app = app();
        app.mouse(wheel(
            MouseEventKind::ScrollRight,
            KeyModifiers::NONE,
            40,
            20,
        ));
        assert!(app.pans.is_empty());
    }

    /// An app with a scroll bar 20 columns wide at column 2 of row 8, under
    /// a block that pans 20 columns. The thumb is 10 columns long.
    fn with_a_scroll_bar() -> App {
        let mut app = app();
        app.bars = vec![ui::ScrollBar {
            area: Rect::new(2, 8, 20, 1),
            key: (1, 0),
            max: 20,
        }];
        app
    }

    fn mouse_at(app: &mut App, kind: MouseEventKind, column: u16) {
        app.mouse(wheel(kind, KeyModifiers::NONE, column, 8));
    }

    fn with_links() -> App {
        let mut app = app();
        app.links =
            toml::from_str("[schemes.foam]\nhover = [\"foam\", \"show\", \"{target}\"]\n").unwrap();
        app.cwd = PathBuf::from("/work");
        let area = |x, key, target: &str| ui::LinkArea {
            area: Rect::new(x, 3, 4, 1),
            key,
            target: target.to_string(),
            title: None,
        };
        app.link_areas = vec![
            area(2, (0, None, 0), "foam:a"),
            area(10, (0, None, 1), "file:src/ui.rs#4"),
            area(20, (0, None, 2), "https://x.test"),
        ];
        app
    }

    fn at(app: &mut App, kind: MouseEventKind, column: u16) -> Action {
        app.mouse(wheel(kind, KeyModifiers::NONE, column, 3))
    }

    #[test]
    fn hovering_a_scheme_link_asks_for_its_command_once() {
        let mut app = with_links();
        let Action::Load { target, argv } = at(&mut app, MouseEventKind::Moved, 3) else {
            panic!("no load");
        };
        assert_eq!(target, "foam:a");
        assert_eq!(argv, ["foam", "show", "a", "--json"]);
        assert_eq!(app.hover_link.as_ref().unwrap().key, (0, None, 0));
        assert!(matches!(
            at(&mut app, MouseEventKind::Moved, 4),
            Action::None
        ));
        assert!(matches!(
            at(&mut app, MouseEventKind::Moved, 40),
            Action::None
        ));
        assert!(app.hover_link.is_none());
        assert!(matches!(
            at(&mut app, MouseEventKind::Moved, 3),
            Action::None
        ));
        app.loaded("foam:a".to_string(), Ok("text".to_string()));
        assert_eq!(app.link_cache["foam:a"], Loaded::Text("text".to_string()));
    }

    #[test]
    fn hovering_a_file_link_runs_nothing_and_opens_the_popup() {
        let mut app = with_links();
        assert!(matches!(
            at(&mut app, MouseEventKind::Moved, 11),
            Action::None
        ));
        assert_eq!(app.hover_link.as_ref().unwrap().target, "file:src/ui.rs#4");
        assert!(app.link_cache.is_empty());
    }

    #[test]
    fn clicking_a_file_link_shows_it_in_decade_or_says_there_is_no_connection() {
        let mut app = with_links();
        assert!(matches!(click(&mut app, 11, 3), Action::None));
        assert_eq!(app.unknown.as_deref(), Some("no decade connection"));
        app.decade_pane = Some(String::new());
        let Action::Show { to, show } = click(&mut app, 11, 3) else {
            panic!("no show");
        };
        assert_eq!(to, "code");
        assert_eq!(show.target, "/work/src/ui.rs");
        assert_eq!(show.at, Some(serde_json::json!(4)));
    }

    #[test]
    fn clicking_a_url_or_a_scheme_with_no_click_command_does_nothing_by_default() {
        let mut app = with_links();
        assert!(matches!(click(&mut app, 21, 3), Action::None));
        assert!(matches!(click(&mut app, 3, 3), Action::None));
        app.links.open_urls = true;
        assert!(matches!(click(&mut app, 21, 3), Action::Run(_)));
    }

    #[test]
    fn pressing_on_the_thumb_and_dragging_moves_the_thumb_with_the_pointer() {
        let mut app = with_a_scroll_bar();
        let down = MouseEventKind::Down(MouseButton::Left);
        let drag = MouseEventKind::Drag(MouseButton::Left);

        // the thumb covers columns 0..10 of the bar, so column 5 is on it
        mouse_at(&mut app, down, 7);
        assert!(app.drag.is_some());
        assert!(!app.pans.contains_key(&(1, 0)));
        mouse_at(&mut app, drag, 12);
        assert_eq!(app.pans[&(1, 0)], 10);
        assert_eq!(ui::thumb(20, 20, 10, 2).0, 5);
        mouse_at(&mut app, drag, 17);
        assert_eq!(app.pans[&(1, 0)], 20);
        mouse_at(&mut app, drag, 40);
        assert_eq!(app.pans[&(1, 0)], 20);
        mouse_at(&mut app, drag, 0);
        assert_eq!(app.pans[&(1, 0)], 0);
    }

    #[test]
    fn a_drag_ends_at_the_release() {
        let mut app = with_a_scroll_bar();
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), 7);
        mouse_at(&mut app, MouseEventKind::Drag(MouseButton::Left), 12);
        mouse_at(&mut app, MouseEventKind::Up(MouseButton::Left), 12);
        mouse_at(&mut app, MouseEventKind::Drag(MouseButton::Left), 20);
        assert_eq!(app.pans[&(1, 0)], 10);
        assert!(app.drag.is_none());
    }

    #[test]
    fn a_click_on_the_track_centres_the_thumb_there() {
        let mut app = with_a_scroll_bar();
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), 12);

        // column 10 of the bar, a thumb 10 long: it starts at column 5
        let offset = app.pans[&(1, 0)];
        assert_eq!(ui::thumb(20, 20, offset, 2).0, 5);
        mouse_at(&mut app, MouseEventKind::Up(MouseButton::Left), 12);
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), 2);
        assert_eq!(app.pans[&(1, 0)], 0);
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), 21);
        assert_eq!(app.pans[&(1, 0)], 20);
    }

    #[test]
    fn a_press_beside_the_bar_pans_nothing() {
        let mut app = with_a_scroll_bar();
        app.mouse(wheel(
            MouseEventKind::Down(MouseButton::Left),
            KeyModifiers::NONE,
            12,
            9,
        ));
        assert!(app.drag.is_none());
        assert!(app.pans.is_empty());
    }

    #[test]
    fn the_pointer_is_over_the_thumb_only_on_the_thumb() {
        let mut app = with_a_scroll_bar();
        mouse_at(&mut app, MouseEventKind::Moved, 4);
        assert_eq!(app.hover_bar, Some((1, 0)));
        mouse_at(&mut app, MouseEventKind::Moved, 18);
        assert_eq!(app.hover_bar, None);
    }

    #[test]
    fn a_wide_block_gets_a_scroll_bar_row_under_it_the_width_it_shows() {
        let mut app = app();
        app.wide.clear();
        let line = "x".repeat(80);
        app.transcript
            .entries
            .push(reply(format!("```rust\n{line}\n```")));
        let screen = header_rows_all(&mut app, 40);
        assert_eq!(app.bars.len(), 1);
        let bar = &app.bars[0];
        assert_eq!(bar.area.x, 2);
        let row: &str = screen.lines().nth(bar.area.y as usize).unwrap();
        assert!(row.contains('━'), "{row}");
        assert!(!screen.contains('◂') && !screen.contains('▸'), "{screen}");
    }

    #[test]
    fn a_link_in_a_panned_table_is_hovered_and_clicked_on_the_columns_it_shows() {
        let mut app = app();
        app.wide.clear();
        let note = "yyyy ".repeat(5);
        app.transcript.entries.push(reply(format!(
            "| name | a | b | c |\n|---|---|---|---|\n| [alpha](file:src/ui.rs#4) | {note} | {note} | {note} |"
        )));
        let text_under = |app: &mut App| {
            let screen = header_rows_all(app, 40);
            assert_eq!(app.link_areas.len(), 1, "{screen}");
            let area = app.link_areas[0].area;
            let row = screen.lines().nth(area.y as usize).unwrap();
            let text: String = row
                .chars()
                .skip(area.x as usize)
                .take(area.width as usize)
                .collect();
            (area, text)
        };
        let (whole, text) = text_under(&mut app);
        assert_eq!(text, "alpha");

        // the box's text starts two columns in, after `│ `
        let col = whole.x as usize - 2;
        app.pans.insert((0, 0), col + 2);
        let (cut, text) = text_under(&mut app);
        assert_eq!(text, "pha");
        assert_eq!(cut.x, 2);
        app.mouse(wheel(
            MouseEventKind::Moved,
            KeyModifiers::NONE,
            cut.x,
            cut.y,
        ));
        assert_eq!(app.hover_link.as_ref().unwrap().key, (0, None, 0));
        click(&mut app, cut.x, cut.y);
        assert_eq!(app.unknown.as_deref(), Some("no decade connection"));
        app.pans.insert((0, 0), col + 5);
        header_rows_all(&mut app, 40);
        assert!(app.link_areas.is_empty());
    }

    #[test]
    fn a_file_link_with_bold_and_code_in_its_text_hovers_and_clicks_on_every_column() {
        let mut app = app();
        app.wide.clear();
        app.transcript
            .entries
            .push(reply("see [**ui**`.rs`](file:src/ui.rs#4) now"));
        let screen = header_rows_all(&mut app, 40);
        assert_eq!(app.link_areas.len(), 1, "{screen}");
        let area = app.link_areas[0].area;
        let row = screen.lines().nth(area.y as usize).unwrap();
        let text: String = row
            .chars()
            .skip(area.x as usize)
            .take(area.width as usize)
            .collect();
        assert_eq!(text, "ui.rs");
        for x in area.x..area.x + area.width {
            app.hover_link = None;
            app.mouse(wheel(MouseEventKind::Moved, KeyModifiers::NONE, x, area.y));
            assert_eq!(app.hover_link.as_ref().unwrap().target, "file:src/ui.rs#4");
            app.unknown = None;
            click(&mut app, x, area.y);
            assert_eq!(app.unknown.as_deref(), Some("no decade connection"));
        }
    }

    #[test]
    fn a_file_link_inside_a_rendered_markdown_block_hovers_and_clicks() {
        let mut app = app();
        app.wide.clear();
        app.transcript.entries.push(reply(
            "see [a](foam:x)\n\n```markdown\nopen [ui](file:src/ui.rs#4)\n```".to_string(),
        ));
        let screen = header_rows_all(&mut app, 60);
        assert_eq!(app.link_areas.len(), 2, "{screen}");
        let area = app.link_areas[1].area;
        let row = screen.lines().nth(area.y as usize).unwrap();
        let text: String = row
            .chars()
            .skip(area.x as usize)
            .take(area.width as usize)
            .collect();
        assert_eq!(text, "ui");
        app.mouse(wheel(
            MouseEventKind::Moved,
            KeyModifiers::NONE,
            area.x,
            area.y,
        ));
        let hover = app.hover_link.as_ref().unwrap();
        assert_eq!(hover.key, (0, Some(1), 0));
        assert_eq!(hover.target, "file:src/ui.rs#4");
        click(&mut app, area.x, area.y);
        assert_eq!(app.unknown.as_deref(), Some("no decade connection"));
    }

    /// An app whose first entry is agent text with one code block, drawn
    /// once so the copy glyph has a place on screen.
    fn with_a_code_block() -> App {
        let mut app = app();
        app.wide.clear();
        app.transcript
            .entries
            .push(reply("```rs\nfn main() {\n    run();\n}\n```".to_string()));
        header(&mut app, 80);
        app
    }

    #[test]
    fn a_click_on_the_copy_glyph_copies_the_whole_block_text_and_marks_it_copied() {
        let mut app = with_a_code_block();
        assert_eq!(app.copies.len(), 1);
        let glyph = app.copies[0].area;
        let Action::Copy(text) = click(&mut app, glyph.x + glyph.width - 1, glyph.y) else {
            panic!("expected a copy");
        };
        assert_eq!(text, "fn main() {\n    run();\n}");
        assert!(matches!(app.copied, Some(((0, 0), _))));
    }

    #[test]
    fn a_click_elsewhere_in_a_code_block_copies_nothing() {
        let mut app = with_a_code_block();
        let glyph = app.copies[0].area;
        assert!(matches!(
            click(&mut app, glyph.x, glyph.y + 1),
            Action::None
        ));
        assert!(app.copied.is_none());
    }

    /// An app whose first entry is one `markdown` block, drawn once so its
    /// toggle words have a place on screen.
    fn with_a_markdown_block() -> App {
        let mut app = app();
        app.wide.clear();
        app.transcript.entries.push(reply(
            "```markdown\n# Title\n\nsome **bold** text\n```".to_string(),
        ));
        header(&mut app, 80);
        app
    }

    fn toggle_area(app: &App, word: Word) -> Rect {
        app.toggles
            .iter()
            .find(|toggle| toggle.word == word)
            .unwrap()
            .area
    }

    #[test]
    fn a_markdown_block_shows_rendered_until_raw_is_clicked_and_again_after_render_is() {
        let mut app = with_a_markdown_block();
        let screen = header_rows_all(&mut app, 80);
        assert!(screen.contains("render raw"), "{screen}");
        assert!(
            screen.contains("│ Title") && !screen.contains("# Title"),
            "{screen}"
        );
        let raw = toggle_area(&app, Word::Raw);
        assert!(matches!(click(&mut app, raw.x, raw.y), Action::None));
        let screen = header_rows_all(&mut app, 80);
        assert!(
            screen.contains("# Title") && screen.contains("**bold**"),
            "{screen}"
        );
        let render = toggle_area(&app, Word::Render);
        click(&mut app, render.x + render.width - 1, render.y);
        let screen = header_rows_all(&mut app, 80);
        assert!(!screen.contains("# Title"), "{screen}");
    }

    #[test]
    fn the_active_toggle_word_is_in_the_accent_colour() {
        let mut app = with_a_markdown_block();
        let colour = |app: &mut App, word: Word| {
            let area = toggle_area(app, word);
            header_buffer(app, 80)[(area.x, area.y)].fg
        };
        let accent = crate::palette::get().accent;
        assert_eq!(colour(&mut app, Word::Render), accent);
        assert_ne!(colour(&mut app, Word::Raw), accent);
        app.raw.insert((0, 0));
        header(&mut app, 80);
        assert_eq!(colour(&mut app, Word::Raw), accent);
    }

    /// An app whose first entry is three adjacent `rust` fences named as
    /// tabs, drawn once at `width`.
    fn with_tabs(width: u16) -> App {
        let mut app = app();
        app.wide.clear();
        app.transcript.entries.push(reply(
            "```rust tab=\"owned\"\nlet a = 1;\n```\n\n```rust tab=\"borrowed\"\nlet b = 2;\n```\n\n```rust tab=\"boxed\"\nlet c = 3;\n```"
                .to_string(),
        ));
        header(&mut app, width);
        app
    }

    #[test]
    fn a_tabbed_box_shows_its_first_tab_until_another_tab_is_clicked() {
        let mut app = with_tabs(80);
        let screen = header_rows_all(&mut app, 80);
        assert!(screen.contains("┌ rust   owned borrowed boxed"), "{screen}");
        assert!(
            screen.contains("let a = 1;") && !screen.contains("let b"),
            "{screen}"
        );
        let borrowed = toggle_area(&app, Word::Tab(1));
        assert!(matches!(
            click(&mut app, borrowed.x, borrowed.y),
            Action::None
        ));
        assert_eq!(app.tabs[&(0, 0)], 1);
        let screen = header_rows_all(&mut app, 80);
        assert!(
            screen.contains("let b = 2;") && !screen.contains("let a"),
            "{screen}"
        );
        let accent = crate::palette::get().accent;
        let shown = toggle_area(&app, Word::Tab(1));
        assert_eq!(header_buffer(&mut app, 80)[(shown.x, shown.y)].fg, accent);
        let owned = toggle_area(&app, Word::Tab(0));
        assert_ne!(header_buffer(&mut app, 80)[(owned.x, owned.y)].fg, accent);
    }

    #[test]
    fn the_copy_glyph_of_a_tabbed_box_takes_the_shown_tab() {
        let mut app = with_tabs(80);
        app.tabs.insert((0, 0), 2);
        header(&mut app, 80);
        let glyph = app.copies[0].area;
        let Action::Copy(text) = click(&mut app, glyph.x + glyph.width - 1, glyph.y) else {
            panic!("expected a copy");
        };
        assert_eq!(text, "let c = 3;");
    }

    #[test]
    fn tab_names_that_do_not_fit_give_way_to_a_count_that_steps_to_the_next() {
        let mut app = with_tabs(32);
        let screen = header_rows_all(&mut app, 32);
        assert!(screen.contains("┌ rust   owned +2"), "{screen}");
        let more = toggle_area(&app, Word::More(1));
        click(&mut app, more.x, more.y);
        let screen = header_rows_all(&mut app, 32);
        assert!(screen.contains("┌ rust   borrowed +2"), "{screen}");
        assert!(screen.contains("let b = 2;"), "{screen}");
        let more = toggle_area(&app, Word::More(2));
        click(&mut app, more.x, more.y);
        let screen = header_rows_all(&mut app, 32);
        assert!(screen.contains("┌ rust   boxed +2"), "{screen}");
        assert!(screen.contains("let c = 3;"), "{screen}");
        assert!(
            app.toggles
                .iter()
                .any(|toggle| toggle.word == Word::More(0))
        );
    }

    #[test]
    fn at_a_narrow_width_the_shown_tab_keeps_its_name_and_wrap_pan_gives_way() {
        let line = "x".repeat(60);
        let text = format!(
            "```rust tab=\"owned\"\n{line}\n```\n\n```rust tab=\"borrowed\"\nlet b = 2;\n```"
        );
        let mut app = app();
        app.wide.clear();
        app.transcript.entries.push(reply(text));
        header(&mut app, 40);
        let screen = header_rows_all(&mut app, 40);
        assert!(screen.contains("┌ rust   owned +1   wrap pan"), "{screen}");
        header(&mut app, 34);
        let screen = header_rows_all(&mut app, 34);
        assert!(screen.contains("┌ rust   owned +1  "), "{screen}");
        assert!(!screen.contains("wrap"), "{screen}");
        header(&mut app, 24);
        let screen = header_rows_all(&mut app, 24);
        assert!(screen.contains("┌ rust   own…   ⧉"), "{screen}");
        assert!(app.toggles.iter().any(|toggle| toggle.word == Word::Tab(0)));
    }

    /// An app whose first entry is one block fenced with `lang` and a line
    /// too long for a 40 column screen, drawn once.
    fn with_a_long_line(lang: &str) -> App {
        let mut app = app();
        app.wide.clear();
        let line = "x".repeat(60);
        app.transcript
            .entries
            .push(reply(format!("```{lang}\n{line}\n```")));
        header(&mut app, 40);
        app
    }

    #[test]
    fn clicking_wrap_and_pan_switches_a_code_block_between_them() {
        let mut app = with_a_long_line("rust");
        let screen = header_rows_all(&mut app, 40);
        assert!(screen.contains("wrap pan"), "{screen}");
        assert_eq!(app.bars.len(), 1);
        let wrap = toggle_area(&app, Word::Wrap);
        assert!(matches!(click(&mut app, wrap.x, wrap.y), Action::None));
        assert!(app.wraps[&(0, 0)]);
        let screen = header_rows_all(&mut app, 40);
        assert!(app.bars.is_empty() && screen.contains('┆'), "{screen}");
        let pan = toggle_area(&app, Word::Pan);
        click(&mut app, pan.x, pan.y);
        header_rows_all(&mut app, 40);
        assert_eq!(app.bars.len(), 1);
    }

    #[test]
    fn a_wrapped_block_draws_continuation_rows_and_no_scroll_bar() {
        for lang in ["", "text"] {
            let mut app = with_a_long_line(lang);
            let screen = header_rows_all(&mut app, 40);
            assert!(screen.contains("wrap pan"), "{screen}");
            assert!(app.bars.is_empty(), "{screen}");
            let rows: Vec<&str> = screen.lines().filter(|row| row.contains('┆')).collect();
            assert_eq!(rows.len(), 2, "{screen}");
            assert!(rows[0].contains("┆ xxx"), "{screen}");
        }
    }

    #[test]
    fn a_code_block_that_fits_has_no_wrap_or_pan_words() {
        let mut app = app();
        app.wide.clear();
        app.transcript
            .entries
            .push(reply("```rust\nfn main() {}\n```".to_string()));
        let screen = header_rows_all(&mut app, 40);
        assert!(!screen.contains("wrap pan"), "{screen}");
    }

    #[test]
    fn the_copy_glyph_of_a_markdown_block_takes_its_source_in_either_view() {
        let source = "# Title\n\nsome **bold** text";
        let mut app = with_a_markdown_block();
        for raw in [false, true] {
            if raw {
                app.raw.insert((0, 0));
                header(&mut app, 80);
            }
            let glyph = app.copies[0].area;
            let Action::Copy(text) = click(&mut app, glyph.x + glyph.width - 1, glyph.y) else {
                panic!("expected a copy");
            };
            assert_eq!(text, source);
        }
    }

    #[test]
    fn a_code_block_in_another_language_has_no_toggle_words() {
        let app = with_a_code_block();
        assert!(app.toggles.is_empty());
    }

    #[test]
    fn the_copy_glyph_is_accent_under_the_pointer_and_says_copied_after_a_click() {
        let mut app = with_a_code_block();
        let glyph = app.copies[0].area;
        let last = glyph.x + glyph.width - 1;
        let cell = |app: &mut App, x: u16| {
            let buffer = header_buffer(app, 80);
            (
                buffer[(x, glyph.y)].symbol().to_string(),
                buffer[(x, glyph.y)].fg,
            )
        };
        let (symbol, colour) = cell(&mut app, last);
        assert_eq!(symbol, "\u{29c9}");
        assert_ne!(colour, crate::palette::get().accent);
        app.mouse(wheel(
            MouseEventKind::Moved,
            KeyModifiers::NONE,
            last,
            glyph.y,
        ));
        assert_eq!(cell(&mut app, last).1, crate::palette::get().accent);
        click(&mut app, last, glyph.y);
        let buffer = header_buffer(&mut app, 80);
        let row: String = (0..80)
            .map(|x| buffer[(x, glyph.y)].symbol().to_string())
            .collect();
        assert!(row.contains("copied"), "{row}");
    }

    #[test]
    fn hovering_an_expandable_calls_head_marks_it_hovered() {
        let mut app = app();
        app.wide.clear();
        app.folds = vec![ui::FoldArea {
            area: Rect::new(0, 4, 60, 2),
            whole: Rect::new(0, 4, 60, 2),
            id: ToolCallId::new("t1"),
        }];
        app.mouse(wheel(MouseEventKind::Moved, KeyModifiers::NONE, 3, 5));
        assert_eq!(app.hover_fold, Some(ToolCallId::new("t1")));
        app.mouse(wheel(MouseEventKind::Moved, KeyModifiers::NONE, 3, 9));
        assert_eq!(app.hover_fold, None);
    }

    #[test]
    fn a_click_on_an_edit_or_run_call_unfolds_it_and_a_second_click_folds_it() {
        use crate::session::Tool;
        use agent_client_protocol::v1::schema::{Diff, ToolCallContent, ToolCallStatus, ToolKind};

        let mut app = with_settings();
        app.wide.clear();
        let mut diff = Diff::new("src/main.rs", "new line");
        diff.old_text = Some("old line".to_string());
        let tool = Tool {
            id: "e1".into(),
            title: "Edit src/main.rs".to_string(),
            kind: ToolKind::Edit,
            status: ToolCallStatus::Completed,
            locations: Vec::new(),
            content: vec![ToolCallContent::Diff(diff)],
            cancelled: false,
            changed: (0, 0),
            revision: 0,
            agent: false,
            children: Vec::new(),
            last_child_update: None,
        };
        app.transcript.entries.push(Entry::Tool(tool));
        let rows = |app: &mut App| header_rows_all(app, 80);
        assert!(!rows(&mut app).contains("old line"));
        let fold = &app.folds[0];
        let (x, y) = (fold.area.x, fold.area.y);
        click(&mut app, x, y);
        assert!(rows(&mut app).contains("- old line"));
        click(&mut app, x, y);
        assert!(!rows(&mut app).contains("old line"));
    }

    #[test]
    fn a_run_of_calls_draws_folded_and_a_click_on_it_unfolds_and_folds_it() {
        use crate::session::Tool;
        use agent_client_protocol::v1::schema::{ToolCallStatus, ToolKind};

        let read = |id: &str, title: &str| {
            Entry::Tool(Tool {
                id: agent_client_protocol::v1::schema::ToolCallId::new(id.to_string()),
                title: title.to_string(),
                kind: ToolKind::Read,
                status: ToolCallStatus::Completed,
                locations: Vec::new(),
                content: Vec::new(),
                cancelled: false,
                changed: (0, 0),
                revision: 0,
                agent: false,
                children: Vec::new(),
                last_child_update: None,
            })
        };
        let mut app = drawn_with(vec![read("r1", "Read one.rs"), read("r2", "Read two.rs")]);
        let rows = |app: &mut App| header_rows_all(app, 80);
        assert!(rows(&mut app).contains("2 tools"));
        assert!(!rows(&mut app).contains("Read one.rs"));
        let fold = &app.folds[0];
        let (x, y) = (fold.area.x, fold.area.y);
        click(&mut app, x, y);
        let open = rows(&mut app);
        assert!(
            open.contains("2 tools") && open.contains("Read one.rs"),
            "{open}"
        );
        assert!(open.contains("Read two.rs"), "{open}");
        click(&mut app, x, y);
        assert!(!rows(&mut app).contains("Read one.rs"));
    }

    #[test]
    fn a_played_click_on_a_folded_runs_text_unfolds_it() {
        use crate::scenario::{InputStep, Target};
        use crate::session::Tool;
        use agent_client_protocol::v1::schema::{ToolCallStatus, ToolKind};

        let read = |id: &str, title: &str| {
            Entry::Tool(Tool {
                id: agent_client_protocol::v1::schema::ToolCallId::new(id.to_string()),
                title: title.to_string(),
                kind: ToolKind::Read,
                status: ToolCallStatus::Completed,
                locations: Vec::new(),
                content: Vec::new(),
                cancelled: false,
                changed: (0, 0),
                revision: 0,
                agent: false,
                children: Vec::new(),
                last_child_update: None,
            })
        };
        let mut app = drawn_with(vec![read("r1", "Read one.rs"), read("r2", "Read two.rs")]);
        let click = InputStep::Click {
            click: Target::Text("2 tools".to_string()),
            ms: Some(50),
        };
        let mut player = Player::new(vec![click]).unwrap();
        let mut now = Instant::now();
        while let Some(due) = player.due() {
            now = due.max(now);
            let screen = header_buffer(&mut app, 80);
            if let Some(TermEvent::Mouse(mouse)) = player.next(now, true, &screen) {
                app.pointer(mouse);
            }
        }
        assert!(header_rows_all(&mut app, 80).contains("Read one.rs"));
    }

    #[test]
    fn an_unfolded_run_draws_its_calls_indented_under_its_row_with_no_blank_rows_between() {
        use crate::session::Tool;
        use agent_client_protocol::v1::schema::{ToolCallStatus, ToolKind};

        let read = |id: &str, title: &str| {
            Entry::Tool(Tool {
                id: agent_client_protocol::v1::schema::ToolCallId::new(id.to_string()),
                title: title.to_string(),
                kind: ToolKind::Read,
                status: ToolCallStatus::Completed,
                locations: Vec::new(),
                content: Vec::new(),
                cancelled: false,
                changed: (0, 0),
                revision: 0,
                agent: false,
                children: Vec::new(),
                last_child_update: None,
            })
        };
        let mut app = drawn_with(vec![read("r1", "Read one.rs"), read("r2", "Read two.rs")]);
        header_rows_all(&mut app, 80);
        let fold = &app.folds[0];
        let (x, y) = (fold.area.x, fold.area.y);
        click(&mut app, x, y);
        let open = header_rows_all(&mut app, 80);
        let lines: Vec<&str> = open.lines().map(str::trim_end).collect();
        let row = lines
            .iter()
            .position(|line| line.contains("2 tools"))
            .unwrap();
        assert!(lines[row].starts_with("▾ ✓ 2 tools  read 2"), "{open}");
        assert_eq!(lines[row + 1], "      ≡ read   Read one.rs", "{open}");
        assert_eq!(lines[row + 2], "      ≡ read   Read two.rs", "{open}");
        assert_eq!(lines[row + 3], "", "{open}");
    }

    fn unfolded_edit(app: &mut App) -> ui::FoldArea {
        use crate::session::Tool;
        use agent_client_protocol::v1::schema::{Diff, ToolCallContent, ToolCallStatus, ToolKind};

        app.wide.clear();
        let mut diff = Diff::new("src/main.rs", "new line");
        diff.old_text = Some("old line".to_string());
        app.transcript.entries.push(Entry::Tool(Tool {
            id: "e1".into(),
            title: "Edit src/main.rs".to_string(),
            kind: ToolKind::Edit,
            status: ToolCallStatus::Completed,
            locations: Vec::new(),
            content: vec![ToolCallContent::Diff(diff)],
            cancelled: false,
            changed: (0, 0),
            revision: 0,
            agent: false,
            children: Vec::new(),
            last_child_update: None,
        }));
        app.expanded.insert("e1".into());
        header_rows_all(app, 80);
        let fold = &app.folds[0];
        ui::FoldArea {
            area: fold.area,
            whole: fold.whole,
            id: fold.id.clone(),
        }
    }

    #[test]
    fn a_click_in_an_unfolded_calls_body_folds_it() {
        let mut app = with_settings();
        let fold = unfolded_edit(&mut app);
        assert!(fold.whole.height > fold.area.height);
        let last = fold.whole.bottom() - 1;
        click(&mut app, fold.whole.x + 2, last);
        assert!(!header_rows_all(&mut app, 80).contains("old line"));
    }

    #[test]
    fn hovering_anywhere_on_a_foldable_call_shades_all_of_it_with_the_tint() {
        let mut app = with_settings();
        let fold = unfolded_edit(&mut app);
        let last = fold.whole.bottom() - 1;
        app.mouse(wheel(
            MouseEventKind::Moved,
            KeyModifiers::NONE,
            fold.whole.x + 2,
            last,
        ));
        assert_eq!(app.hover_fold, Some(fold.id.clone()));
        let buffer = header_buffer(&mut app, 80);
        let tint = crate::palette::get().tint;
        assert_eq!(buffer[(fold.whole.x, fold.whole.y)].bg, tint);
        assert_eq!(buffer[(fold.whole.x + 5, last)].bg, tint);
        assert_ne!(buffer[(fold.whole.x, fold.whole.bottom())].bg, tint);
    }

    fn typed(app: &mut App, text: &str) {
        for ch in text.chars() {
            app.key(
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                Instant::now(),
            );
        }
    }

    #[test]
    fn enter_on_an_unknown_command_names_it_and_keeps_the_text() {
        let mut app = with_settings();
        typed(&mut app, "/nope");
        app.key(enter(), Instant::now());
        assert_eq!(app.unknown.as_deref(), Some("no command /nope"));
        assert_eq!(app.input, "/nope");
        assert!(app.menu.is_none());
    }

    #[test]
    fn enter_on_an_unknown_agent_command_names_it() {
        let mut app = with_settings();
        typed(&mut app, "//nope");
        app.key(enter(), Instant::now());
        assert_eq!(app.unknown.as_deref(), Some("no agent command //nope"));
        assert_eq!(app.input, "//nope");
    }

    #[test]
    fn the_unknown_command_line_shows_under_the_prompt_until_the_next_key() {
        let mut app = with_settings();
        typed(&mut app, "/nope");
        app.key(enter(), Instant::now());
        let rows = header_rows_all(&mut app, 60);
        let last = rows.lines().nth_back(4).unwrap();
        assert!(last.starts_with("━━ no command /nope ━"), "{last}");
        typed(&mut app, "x");
        assert!(app.unknown.is_none());
        let rows = header_rows_all(&mut app, 60);
        assert!(!rows.contains("no command"), "{rows}");
    }

    #[test]
    fn a_known_command_names_nothing_unknown() {
        let mut app = with_settings();
        typed(&mut app, "/fast");
        app.key(enter(), Instant::now());
        assert!(app.unknown.is_none());
    }

    fn with_a_long_conversation() -> App {
        let mut app = with_settings();
        for n in 0..60 {
            app.transcript.notice(format!("notice {n}"));
        }
        header(&mut app, 80);
        app
    }

    #[test]
    fn a_short_conversation_has_no_scroll_bar() {
        let mut app = with_settings();
        app.transcript.notice("one".to_string());
        header(&mut app, 80);
        assert!(app.v_bar.is_none());
    }

    #[test]
    fn a_tall_conversation_shows_a_bar_in_the_last_column_of_the_transcript() {
        let mut app = with_a_long_conversation();
        let bar = app.v_bar.as_ref().expect("the bar shows");
        assert_eq!(bar.area.x, 79);
        assert_eq!(bar.area.width, 1);
        let (y, height) = (bar.area.y, bar.area.height);
        let (start, len) = bar.thumb(app.scroll);
        let screen = header_buffer(&mut app, 80);
        let column: Vec<String> = (y..y + height)
            .map(|row| screen[(79, row)].symbol().to_string())
            .collect();
        for (row, symbol) in column.iter().enumerate() {
            let on = (start..start + len).contains(&row);
            assert_eq!(symbol, if on { "┃" } else { "│" }, "row {row}");
        }

        // not scrolled, so the thumb is at the bottom
        assert_eq!(start + len, height as usize);
    }

    #[test]
    fn the_transcript_is_laid_out_one_column_narrower_while_the_bar_shows() {
        // a wide block fills its box, which is 90% of the width
        let right_edge = |app: &mut App| {
            let screen = header_buffer(app, 80);
            (0..80u16)
                .rev()
                .find(|x| (0..30).any(|y| screen[(*x, y)].symbol() == "╮"))
        };
        let mut app = with_settings();
        let long = "x".repeat(200);
        app.transcript
            .entries
            .push(reply(format!("```\n{long}\n```")));
        assert_eq!(right_edge(&mut app), Some(71));
        assert!(app.v_bar.is_none());
        for n in 0..60 {
            app.transcript.notice(format!("notice {n}"));
        }
        app.scroll = 1000;
        assert_eq!(right_edge(&mut app), Some(70));
        assert!(app.v_bar.is_some());
    }

    fn with_a_scroll_bar_of(max: usize) -> App {
        let mut app = app();
        app.v_bar = Some(ui::VBar {
            area: Rect::new(79, 2, 1, 20),
            max,
        });
        app
    }

    #[test]
    fn the_conversations_thumb_is_the_visible_share_and_sits_at_the_bottom_when_not_scrolled() {
        let bar = ui::VBar {
            area: Rect::new(79, 2, 1, 20),
            max: 80,
        };
        assert_eq!(bar.thumb(0), (16, 4));
        assert_eq!(bar.thumb(40), (8, 4));
        assert_eq!(bar.thumb(80), (0, 4));
        let huge = ui::VBar {
            area: Rect::new(79, 2, 1, 20),
            max: 10_000,
        };
        assert_eq!(huge.thumb(0), (19, 1));
        assert_eq!(huge.thumb(10_000), (0, 1));
    }

    #[test]
    fn the_scroll_for_a_thumb_position_inverts_the_thumb() {
        let bar = ui::VBar {
            area: Rect::new(79, 2, 1, 20),
            max: 80,
        };
        assert_eq!(bar.scroll_for_thumb(16), 0);
        assert_eq!(bar.scroll_for_thumb(8), 40);
        assert_eq!(bar.scroll_for_thumb(0), 80);
        assert_eq!(bar.scroll_for_thumb(-5), 80);
        assert_eq!(bar.scroll_for_thumb(99), 0);
    }

    fn mouse_row(app: &mut App, kind: MouseEventKind, row: u16) {
        app.mouse(wheel(kind, KeyModifiers::NONE, 79, row));
    }

    #[test]
    fn pressing_on_the_conversations_thumb_and_dragging_scrolls_with_the_pointer() {
        let mut app = with_a_scroll_bar_of(80);
        let down = MouseEventKind::Down(MouseButton::Left);
        let drag = MouseEventKind::Drag(MouseButton::Left);

        // the thumb is at rows 16..20 of the bar, which starts at row 2
        mouse_row(&mut app, down, 2 + 17);
        assert_eq!(app.v_drag, Some(1));
        assert_eq!(app.scroll, 0);
        mouse_row(&mut app, drag, 2 + 9);
        assert_eq!(app.scroll, 40);
        mouse_row(&mut app, drag, 0);
        assert_eq!(app.scroll, 80);
        mouse_row(&mut app, drag, 40);
        assert_eq!(app.scroll, 0);
        mouse_row(&mut app, MouseEventKind::Up(MouseButton::Left), 40);
        assert_eq!(app.v_drag, None);
        mouse_row(&mut app, drag, 2 + 5);
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn a_click_on_the_conversations_track_centres_the_thumb_there() {
        let mut app = with_a_scroll_bar_of(80);
        mouse_row(&mut app, MouseEventKind::Down(MouseButton::Left), 2 + 3);

        // the thumb is 4 long, so it starts 2 rows above the click
        assert_eq!(app.scroll, 75);
        assert_eq!(app.v_bar.as_ref().unwrap().thumb(app.scroll).0, 1);
        assert_eq!(app.v_drag, Some(2));
    }

    #[test]
    fn a_press_beside_the_conversations_bar_scrolls_nothing() {
        let mut app = with_a_scroll_bar_of(80);
        app.mouse(wheel(
            MouseEventKind::Down(MouseButton::Left),
            KeyModifiers::NONE,
            70,
            10,
        ));
        assert_eq!(app.v_drag, None);
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn the_pointer_is_over_the_conversations_thumb_only_on_the_thumb() {
        let mut app = with_a_scroll_bar_of(80);
        mouse_row(&mut app, MouseEventKind::Moved, 2 + 18);
        assert!(app.hover_v_bar);
        mouse_row(&mut app, MouseEventKind::Moved, 2 + 5);
        assert!(!app.hover_v_bar);
    }

    #[test]
    fn the_thumb_follows_the_scroll_while_new_lines_arrive_under_a_scrolled_view() {
        let mut app = with_a_long_conversation();
        app.scroll = 10;
        header(&mut app, 80);
        let before = app.v_bar.as_ref().unwrap().thumb(app.scroll);
        let first = app.scroll;
        app.transcript.notice("late".to_string());
        header(&mut app, 80);

        // the view holds still, so the scroll offset grows with the new lines
        assert!(app.scroll > first);
        let after = app.v_bar.as_ref().unwrap().thumb(app.scroll);
        assert!(after.0 <= before.0);
    }

    #[test]
    fn the_wheel_and_the_page_keys_still_scroll_with_the_bar_showing() {
        let mut app = with_a_long_conversation();
        app.mouse(wheel(MouseEventKind::ScrollUp, KeyModifiers::NONE, 3, 6));
        assert_eq!(app.scroll, 3);
        app.key(
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
            Instant::now(),
        );
        assert!(app.scroll > 3);
        app.key(
            KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
            Instant::now(),
        );
        assert_eq!(app.scroll, 0);
    }

    fn ctrl(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }

    fn open_permission(app: &mut App) {
        let (reply, _answer) = oneshot::channel();
        app.permission = Some(Permission {
            changed: (0, 0),
            title: "Write".to_string(),
            options: Vec::new(),
            update: Box::new(ToolCallUpdate::new("t1", Default::default())),
            reply,
        });
    }

    #[test]
    fn ctrl_c_while_the_agent_is_working_cancels_the_turn_and_arms_the_guard() {
        let mut app = app();
        app.transcript.in_flight = true;
        let start = Instant::now();
        assert!(matches!(app.key(ctrl('c'), start), Action::Cancel));
        assert!(app.guard.is_some());

        // an agent that ignores the cancel leaves the turn in flight
        assert!(matches!(app.key(ctrl('c'), start), Action::Quit));
    }

    #[test]
    fn a_second_ctrl_c_after_the_window_cancels_again_while_the_agent_works() {
        let mut app = app();
        app.transcript.in_flight = true;
        let start = Instant::now();
        app.key(ctrl('c'), start);
        let later = start + GUARD_WINDOW + Duration::from_millis(1);
        assert!(matches!(app.key(ctrl('c'), later), Action::Cancel));
    }

    #[test]
    fn ctrl_c_with_text_in_the_prompt_clears_it() {
        let mut app = app();
        app.input = "half a thought".to_string();
        assert!(matches!(app.key(ctrl('c'), Instant::now()), Action::None));
        assert!(app.input.is_empty());
        assert!(app.guard.is_none());
    }

    #[test]
    fn ctrl_c_on_an_idle_empty_prompt_arms_and_a_second_one_inside_the_window_quits() {
        let mut app = app();
        let start = Instant::now();
        assert!(matches!(app.key(ctrl('c'), start), Action::None));
        assert_eq!(app.guard.map(|guard| guard.key), Some(GuardKey::CtrlC));
        let second = start + Duration::from_millis(1400);
        assert!(matches!(app.key(ctrl('c'), second), Action::Quit));
    }

    #[test]
    fn a_second_ctrl_c_after_the_window_arms_again_instead_of_quitting() {
        let mut app = app();
        let start = Instant::now();
        app.key(ctrl('c'), start);
        let late = start + Duration::from_millis(1600);
        assert!(matches!(app.key(ctrl('c'), late), Action::None));
        assert_eq!(
            app.guard.map(|guard| guard.until),
            Some(late + GUARD_WINDOW)
        );
    }

    #[test]
    fn ctrl_c_with_the_permission_box_open_arms_and_a_second_quits() {
        let mut app = app();
        app.transcript.in_flight = true;
        open_permission(&mut app);
        let start = Instant::now();
        assert!(matches!(app.key(ctrl('c'), start), Action::None));
        assert!(app.guard.is_some());
        assert!(matches!(app.key(ctrl('c'), start), Action::Quit));
    }

    #[test]
    fn ctrl_d_on_an_empty_prompt_arms_its_own_guard_and_a_second_quits() {
        let mut app = app();
        let start = Instant::now();
        assert!(matches!(app.key(ctrl('d'), start), Action::None));
        assert_eq!(app.guard.map(|guard| guard.key), Some(GuardKey::CtrlD));
        assert!(matches!(app.key(ctrl('d'), start), Action::Quit));
    }

    #[test]
    fn ctrl_d_with_text_in_the_prompt_does_nothing() {
        let mut app = app();
        app.input = "text".to_string();
        assert!(matches!(app.key(ctrl('d'), Instant::now()), Action::None));
        assert!(app.guard.is_none());
        assert_eq!(app.input, "text");
    }

    #[test]
    fn the_other_exit_key_does_not_complete_the_guard() {
        let mut app = app();
        let start = Instant::now();
        app.key(ctrl('c'), start);
        assert!(matches!(app.key(ctrl('d'), start), Action::None));
        assert_eq!(app.guard.map(|guard| guard.key), Some(GuardKey::CtrlD));
    }

    #[test]
    fn any_other_key_disarms_the_guard() {
        let mut app = app();
        let start = Instant::now();
        app.key(ctrl('c'), start);
        app.key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE), start);
        assert!(app.guard.is_none());
        assert!(matches!(app.key(ctrl('c'), start), Action::None));
        assert!(app.guard.is_some());
    }

    fn esc() -> KeyEvent {
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
    }

    #[test]
    fn a_first_esc_during_a_turn_arms_the_cancel_guard_and_does_not_cancel() {
        let mut app = app();
        app.transcript.in_flight = true;
        assert!(matches!(app.key(esc(), Instant::now()), Action::None));
        assert_eq!(app.guard.map(|guard| guard.key), Some(GuardKey::Esc));
        assert!(app.transcript.in_flight);
    }

    #[test]
    fn a_second_esc_inside_the_window_cancels_the_turn() {
        let mut app = app();
        app.transcript.in_flight = true;
        let start = Instant::now();
        app.key(esc(), start);
        let second = start + Duration::from_millis(1400);
        assert!(matches!(app.key(esc(), second), Action::Cancel));
    }

    #[test]
    fn another_key_between_the_two_escs_disarms_the_cancel_guard() {
        let mut app = app();
        app.transcript.in_flight = true;
        let start = Instant::now();
        app.key(esc(), start);
        app.key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE), start);
        assert!(app.guard.is_none());
        assert!(matches!(app.key(esc(), start), Action::None));
        assert_eq!(app.guard.map(|guard| guard.key), Some(GuardKey::Esc));
    }

    #[test]
    fn an_esc_after_the_window_arms_the_cancel_guard_again() {
        let mut app = app();
        app.transcript.in_flight = true;
        let start = Instant::now();
        app.key(esc(), start);
        let late = start + Duration::from_millis(1600);
        assert!(matches!(app.key(esc(), late), Action::None));
        assert_eq!(
            app.guard.map(|guard| guard.until),
            Some(late + GUARD_WINDOW)
        );
    }

    #[test]
    fn esc_on_an_open_form_during_a_turn_declines_it_at_once() {
        let mut app = with_settings();
        app.transcript.prompt("hi".to_string());
        let mut answer = ask_form(&mut app);
        assert!(matches!(app.key(esc(), Instant::now()), Action::None));
        assert!(app.form.is_none());
        assert!(app.guard.is_none());
        assert!(matches!(answer.try_recv(), Ok(ElicitationAction::Decline)));
    }

    #[test]
    fn ctrl_c_cancels_a_running_turn_on_one_press_with_the_esc_guard_armed() {
        let mut app = app();
        app.transcript.in_flight = true;
        let start = Instant::now();
        app.key(esc(), start);
        assert!(matches!(app.key(ctrl('c'), start), Action::Cancel));
        assert_eq!(app.guard.map(|guard| guard.key), Some(GuardKey::CtrlC));
    }

    #[test]
    fn the_rule_under_the_prompt_drops_the_message_once_the_window_ends() {
        let mut app = with_settings();
        app.transcript.in_flight = true;
        app.key(esc(), Instant::now());
        assert!(header_rows_all(&mut app, 100).contains("press esc again to cancel"));
        app.guard = Some(Guard {
            key: GuardKey::Esc,
            until: Instant::now(),
        });
        assert!(!header_rows_all(&mut app, 100).contains("again to cancel"));
    }

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::scenario::Scenario;

    fn with_settings() -> App {
        let scenario = Scenario::parse(include_str!("../scenarios/resume.json")).unwrap();
        let mut app = app();
        app.wide.clear();
        app.settings.open(Some(scenario.config_options), None);
        app
    }

    fn header_buffer(app: &mut App, width: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
        terminal.draw(|frame| ui::draw(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    const WORKING_ROW: u16 = 22;
    const CONTEXT_ROW: u16 = 26;
    const STATUS_ROW: u16 = 27;
    const PROGRESS_ROW: u16 = 28;
    const EDGE_ROW: u16 = 29;

    fn screen_row(app: &mut App, width: u16, y: u16) -> String {
        let buffer = header_buffer(app, width);
        (0..width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    }

    fn status_row(app: &mut App, width: u16) -> String {
        screen_row(app, width, STATUS_ROW)
    }

    fn progress_row(app: &mut App, width: u16) -> String {
        screen_row(app, width, PROGRESS_ROW)
    }

    fn header_rows_all(app: &mut App, width: u16) -> String {
        let buffer = header_buffer(app, width);
        (0..30)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn header(app: &mut App, width: u16) -> String {
        status_row(app, width)
    }

    #[test]
    fn the_footer_shows_the_context_then_the_status_line_then_the_progress_then_the_edge() {
        use agent_client_protocol::v1::schema::{Cost, SessionUpdate, Usage, UsageUpdate};

        let mut app = with_settings();
        let update = UsageUpdate::new(28_481, 1_000_000).cost(Cost::new(0.136, "USD"));
        app.settings.apply(&SessionUpdate::UsageUpdate(update));
        app.settings
            .used(&Usage::new(28_485, 12, 4).cached_read_tokens(28_469));
        let context = screen_row(&mut app, 120, CONTEXT_ROW);
        assert!(context.starts_with(" context: 3%  ━━━──"), "{context}");
        assert_eq!(context.chars().filter(|ch| *ch == '━').count(), 3);
        assert_eq!(context.chars().count(), 120);
        let status = status_row(&mut app, 120);
        assert!(
            status.starts_with(" ◆ auto · ● opus · ◐ effort default · fast off  "),
            "{status}"
        );
        assert!(status.ends_with("↑28k ↓4 · $0.14 · idle "), "{status}");
        assert_eq!(progress_row(&mut app, 120).trim(), "");
        assert_eq!(screen_row(&mut app, 120, EDGE_ROW), "─".repeat(120));
        assert_eq!(app.chips.len(), 4);
    }

    #[test]
    fn each_chip_area_covers_its_label_on_the_status_line() {
        let mut app = with_settings();
        for width in [120, 36] {
            let row: Vec<char> = status_row(&mut app, width).chars().collect();
            let labels: Vec<String> = app
                .chips
                .iter()
                .map(|chip| {
                    let x = chip.area.x as usize;
                    assert_eq!(chip.area.y, STATUS_ROW);
                    row[x..x + chip.area.width as usize].iter().collect()
                })
                .collect();
            let expected: &[&str] = if width == 120 {
                &["◆ auto", "● opus", "◐ effort default", "fast off"]
            } else {
                &["◆ auto", "● opus", "⋯"]
            };
            assert_eq!(labels, expected);
        }
    }

    #[test]
    fn the_conversation_starts_on_the_first_row_with_no_header() {
        let mut app = with_settings();
        app.transcript.notice("first".to_string());
        let screen = header_rows_all(&mut app, 60);
        assert!(screen.lines().next().unwrap().contains("first"), "{screen}");
        assert!(!screen.contains("coax"));
    }

    #[test]
    fn the_footer_rows_have_no_background_and_end_on_a_thin_rule() {
        let mut app = with_settings();
        let buffer = header_buffer(&mut app, 100);
        for x in 0..100 {
            for y in CONTEXT_ROW..=EDGE_ROW {
                assert_eq!(buffer[(x, y)].bg, ratatui::style::Color::Reset, "{x}, {y}");
            }
            assert_eq!(buffer[(x, EDGE_ROW)].symbol(), "─");
        }
    }

    fn is_dim(cell: &ratatui::buffer::Cell) -> bool {
        match crate::palette::get().muted {
            Some(muted) => cell.fg == muted,
            None => cell.modifier.contains(ratatui::style::Modifier::DIM),
        }
    }

    /// The column at which `text` starts on screen row `y`.
    fn column(app: &mut App, width: u16, y: u16, text: &str) -> u16 {
        let line = screen_row(app, width, y);
        let index = line
            .find(text)
            .unwrap_or_else(|| panic!("{text} in {line}"));
        line[..index].chars().count() as u16
    }

    #[test]
    fn the_settings_on_the_status_line_are_coloured_by_what_they_set() {
        let palette = crate::palette::get();
        let mut app = with_settings();
        let buffer = header_buffer(&mut app, 120);
        let at = |n: usize| &buffer[(app.chips[n].area.x, STATUS_ROW)];
        assert_eq!(at(0).fg, palette.yellow);
        assert!(!is_dim(at(0)));
        assert_eq!(at(1).fg, palette.blue);
        assert_eq!(at(2).fg, palette.magenta);
        assert!(is_dim(at(3)));
        let row = status_row(&mut app, 120);
        let dot = row[..row.find('·').unwrap()].chars().count() as u16;
        assert!(is_dim(&buffer[(dot, STATUS_ROW)]));
    }

    #[test]
    fn a_mode_in_which_the_agent_asks_is_cyan_on_the_status_line() {
        use agent_client_protocol::v1::schema::{SessionMode, SessionModeState};

        let mut app = app();
        let modes = SessionModeState::new("plan", vec![SessionMode::new("plan", "Plan")]);
        app.settings.open(None, Some(modes));
        let buffer = header_buffer(&mut app, 120);
        assert_eq!(
            buffer[(app.chips[0].area.x, STATUS_ROW)].fg,
            crate::palette::get().cyan
        );
    }

    #[test]
    fn the_model_on_the_status_line_is_dim_during_a_turn() {
        let mut app = with_settings();
        app.transcript.prompt("hi".to_string());
        let buffer = header_buffer(&mut app, 120);
        assert!(is_dim(&buffer[(app.chips[1].area.x, STATUS_ROW)]));
        assert_eq!(
            buffer[(app.chips[2].area.x, STATUS_ROW)].fg,
            crate::palette::get().magenta
        );
    }

    #[test]
    fn the_session_items_on_the_status_line_are_coloured_by_what_they_show() {
        use agent_client_protocol::v1::schema::{Cost, SessionUpdate, Usage, UsageUpdate};

        let palette = crate::palette::get();
        let mut app = with_settings();
        let update = UsageUpdate::new(28_481, 1_000_000).cost(Cost::new(0.136, "USD"));
        app.settings.apply(&SessionUpdate::UsageUpdate(update));
        app.settings
            .used(&Usage::new(28_485, 12, 4).cached_read_tokens(28_469));
        app.decade_pane = Some(String::new());
        app.update = Some("0.9.0".to_string());

        let width = 140;
        let buffer = header_buffer(&mut app, width);
        let mut cell = |text: &str, offset: u16| {
            let x = column(&mut app, width, STATUS_ROW, text) + offset;
            buffer[(x, STATUS_ROW)].clone()
        };
        assert_eq!(cell("↑28k", 0).fg, palette.cyan);
        assert_eq!(cell("↑28k", 1).fg, palette.foreground);
        assert!(!is_dim(&cell("↑28k", 1)));
        assert_eq!(cell("↓4", 0).fg, palette.green);
        assert_eq!(cell("↓4", 1).fg, palette.foreground);
        assert_eq!(cell("$0.14", 0).fg, palette.orange);
        assert_eq!(cell("0.9.0 available", 0).fg, palette.accent);
        assert!(is_dim(&cell("idle", 0)));
        assert!(is_dim(&cell(" · idle", 1)));
        assert!(!status_row(&mut app, width).contains("decade"));
    }

    fn plan_entry(
        content: &str,
        status: agent_client_protocol::v1::schema::PlanEntryStatus,
    ) -> agent_client_protocol::v1::schema::PlanEntry {
        use agent_client_protocol::v1::schema::{PlanEntry, PlanEntryPriority};

        PlanEntry::new(content, PlanEntryPriority::Medium, status)
    }

    #[test]
    fn the_progress_row_shows_the_plan_and_the_step_in_progress_coloured_by_what_remains() {
        use agent_client_protocol::v1::schema::PlanEntryStatus;

        let palette = crate::palette::get();
        let mut app = with_settings();
        app.transcript.plan = vec![
            plan_entry("read the code", PlanEntryStatus::Completed),
            plan_entry("run the tests", PlanEntryStatus::InProgress),
            plan_entry("write it up", PlanEntryStatus::Pending),
        ];
        let row = progress_row(&mut app, 100);
        assert_eq!(row.trim_end(), " ▸ plan 1/3 · run the tests");
        let buffer = header_buffer(&mut app, 100);
        assert_eq!(buffer[(1, PROGRESS_ROW)].fg, palette.yellow);
        let step = column(&mut app, 100, PROGRESS_ROW, "run");
        assert_eq!(buffer[(step, PROGRESS_ROW)].fg, palette.foreground);

        app.transcript.plan[1].status = PlanEntryStatus::Completed;
        let row = progress_row(&mut app, 100);
        assert_eq!(row.trim_end(), " ▸ plan 2/3 · write it up");

        app.transcript.plan[2].status = PlanEntryStatus::Completed;
        let row = progress_row(&mut app, 100);
        assert_eq!(row.trim_end(), " ▸ plan 3/3");
        let buffer = header_buffer(&mut app, 100);
        assert_eq!(buffer[(1, PROGRESS_ROW)].fg, palette.green);
    }

    #[test]
    fn the_plan_rows_arrow_points_down_while_the_plan_is_shown() {
        use agent_client_protocol::v1::schema::PlanEntryStatus;

        let mut app = with_settings();
        app.transcript.plan = vec![plan_entry("read the code", PlanEntryStatus::Pending)];
        assert!(progress_row(&mut app, 100).starts_with(" ▸ plan 0/1"));
        app.show_plan = true;
        assert!(progress_row(&mut app, 100).starts_with(" ▾ plan 0/1"));
    }

    #[test]
    fn the_progress_row_shows_the_running_tool_during_a_turn_and_the_last_turn_when_idle() {
        use agent_client_protocol::v1::schema::{PlanEntryStatus, SessionUpdate, ToolCall};

        let mut app = with_settings();
        app.transcript.prompt("hi".to_string());
        app.transcript.apply(SessionUpdate::ToolCall(ToolCall::new(
            "t1",
            "Read src/main.rs",
        )));
        let row = progress_row(&mut app, 80);
        assert!(row.ends_with(" Read src/main.rs "), "{row}");
        let buffer = header_buffer(&mut app, 80);
        let x = column(&mut app, 80, PROGRESS_ROW, "Read");
        assert!(is_dim(&buffer[(x, PROGRESS_ROW)]));

        app.transcript.plan = vec![plan_entry(
            "a step with a long description that runs on and on past the edge",
            PlanEntryStatus::InProgress,
        )];
        let row = progress_row(&mut app, 60);
        assert!(row.starts_with(" ▸ plan 0/1 · a step with"), "{row}");
        assert!(row.contains("…  "), "{row}");
        assert!(row.ends_with(" Read src/main.rs "), "{row}");

        app.transcript
            .finished(agent_client_protocol::v1::schema::StopReason::EndTurn);
        let mut reply = Message::new("done".to_string(), None);
        reply.took = Some(std::time::Duration::from_secs(41));
        app.transcript.entries.push(Entry::Agent(reply));
        let row = progress_row(&mut app, 80);
        assert!(row.ends_with(" last turn 41s "), "{row}");
    }

    fn running_task(id: &'static str) -> agent_client_protocol::v1::schema::SessionUpdate {
        use agent_client_protocol::v1::schema::{SessionUpdate, ToolCall, ToolCallStatus};

        let mut meta = serde_json::Map::new();
        meta.insert("claudeCode".to_string(), json!({ "toolName": "Task" }));
        let mut call = ToolCall::new(id, "Survey").status(ToolCallStatus::InProgress);
        call.meta = Some(meta);
        SessionUpdate::ToolCall(call)
    }

    #[test]
    fn the_working_line_shows_the_turn_and_the_running_subagents_side_by_side_in_their_colours() {
        let mut app = with_settings();
        app.transcript.prompt("hi".to_string());
        app.transcript.apply(running_task("t1"));
        app.transcript.apply(running_task("t2"));
        let row = working_row(&mut app, 100);
        assert!(
            row.starts_with(" ◐ working · 0s  ◴ 2 agents running · esc esc to cancel "),
            "{row}"
        );
        let palette = crate::palette::get();
        let buffer = header_buffer(&mut app, 100);
        let working = column(&mut app, 100, WORKING_ROW, "◐");
        assert_eq!(buffer[(working, WORKING_ROW)].fg, palette.yellow);
        let agents = column(&mut app, 100, WORKING_ROW, "◴");
        assert_eq!(buffer[(agents, WORKING_ROW)].fg, palette.cyan);
        assert_eq!(buffer[(agents + 2, WORKING_ROW)].fg, palette.cyan);
    }

    #[test]
    fn the_working_line_shows_the_running_subagents_alone_after_the_turn_ends() {
        let mut app = with_settings();
        app.transcript.prompt("hi".to_string());
        app.transcript.apply(running_task("t1"));
        app.transcript
            .finished(agent_client_protocol::v1::schema::StopReason::EndTurn);
        assert_eq!(working_row(&mut app, 100).trim_end(), " ◴ 1 agent running");
        app.spinner = 1;
        assert_eq!(working_row(&mut app, 100).trim_end(), " ◷ 1 agent running");
        let x = column(&mut app, 100, WORKING_ROW, "1 agent");
        let buffer = header_buffer(&mut app, 100);
        assert_eq!(buffer[(x, WORKING_ROW)].fg, crate::palette::get().cyan);
    }

    fn agent_says(app: &mut App, text: &str) {
        use agent_client_protocol::v1::schema::{ContentChunk, SessionUpdate};

        app.event(Event::Update(Box::new(SessionUpdate::AgentMessageChunk(
            ContentChunk::new(text.into()),
        ))));
    }

    #[test]
    fn a_held_turn_shows_waiting_on_its_agents_in_cyan_without_the_turn_spinner() {
        let mut app = with_settings();
        app.transcript.prompt("hi".to_string());
        app.transcript.apply(running_task("t1"));
        agent_says(&mut app, "started a survey");
        assert_eq!(
            working_row(&mut app, 100).trim_end(),
            " ◴ waiting on 1 agent · esc esc to cancel"
        );
        let x = column(&mut app, 100, WORKING_ROW, "waiting");
        let buffer = header_buffer(&mut app, 100);
        assert_eq!(buffer[(x, WORKING_ROW)].fg, crate::palette::get().cyan);
        app.transcript.apply(running_task("t2"));
        agent_says(&mut app, "and another");
        assert!(working_row(&mut app, 100).starts_with(" ◴ waiting on 2 agents · "));
        assert!(matches!(press(&mut app, KeyCode::Esc), Action::None));
        assert!(matches!(press(&mut app, KeyCode::Esc), Action::Cancel));
    }

    #[test]
    fn the_status_line_says_agents_in_cyan_while_subagents_run_after_the_turn() {
        let mut app = with_settings();
        app.transcript.prompt("hi".to_string());
        app.transcript.apply(running_task("t1"));
        done(
            &mut app,
            agent_client_protocol::v1::schema::StopReason::EndTurn,
        );
        let status = status_row(&mut app, 100);
        assert!(status.ends_with(" agents "), "{status}");
        assert!(!status.contains("idle"), "{status}");
        let x = column(&mut app, 100, STATUS_ROW, "agents");
        let buffer = header_buffer(&mut app, 100);
        assert_eq!(buffer[(x, STATUS_ROW)].fg, crate::palette::get().cyan);
    }

    #[test]
    fn a_narrow_status_line_keeps_the_first_setting_and_ends_the_settings_in_the_overflow_mark() {
        let mut app = with_settings();
        let line = status_row(&mut app, 30);
        assert!(line.starts_with(" ◆ auto"), "{line}");
        assert!(line.contains('⋯'), "{line}");
        assert!(line.trim_end().ends_with("idle"), "{line}");
    }

    #[test]
    fn the_overflow_mark_replaces_the_settings_that_do_not_fit() {
        let mut app = with_settings();
        let line = status_row(&mut app, 36);
        assert!(line.contains('⋯'), "{line}");
        assert!(!line.contains("fast off"), "{line}");
        assert_eq!(
            app.chips.last().unwrap().target,
            ChipTarget::Overflow(app.chips.len() - 1)
        );
    }

    #[test]
    fn a_hovered_setting_turns_accent_and_nothing_else_changes() {
        let mut app = with_settings();
        header_buffer(&mut app, 120);
        let area = app.chips[2].area;
        let before = header_buffer(&mut app, 120);
        assert_ne!(
            before[(area.x, STATUS_ROW)].fg,
            crate::palette::get().accent
        );
        app.mouse(wheel(
            MouseEventKind::Moved,
            KeyModifiers::NONE,
            area.x + 1,
            area.y,
        ));
        let buffer = header_buffer(&mut app, 120);
        for x in area.x..area.x + area.width {
            assert_eq!(buffer[(x, STATUS_ROW)].fg, crate::palette::get().accent);
        }
        assert_eq!(
            buffer[(app.chips[3].area.x, STATUS_ROW)],
            before[(app.chips[3].area.x, STATUS_ROW)]
        );
    }

    /// The row above the upper rule, on a 30-row screen with a one-row
    /// prompt.
    fn working_row(app: &mut App, width: u16) -> String {
        let buffer = header_buffer(app, width);
        (0..width)
            .map(|x| buffer[(x, WORKING_ROW)].symbol().to_string())
            .collect()
    }

    #[test]
    fn a_running_turn_shows_the_spinner_time_and_cancel_key_above_the_prompt() {
        let mut app = with_settings();
        app.transcript.prompt("hi".to_string());
        assert!(working_row(&mut app, 100).starts_with(" ◐ working · 0s · esc esc to cancel "));
        app.spinner = 1;
        assert!(working_row(&mut app, 100).starts_with(" ◓ working "));
        app.spinner = 5;
        assert!(working_row(&mut app, 100).starts_with(" ◓ working "));
        let status = status_row(&mut app, 100);
        assert!(!status.contains("working") && !status.contains("idle"));
    }

    fn done(app: &mut App, stop: agent_client_protocol::v1::schema::StopReason) {
        app.event(Event::Done(Ok(stop)));
    }

    fn users(app: &App) -> Vec<&str> {
        app.transcript
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::User(prompt) => Some(prompt.text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_prompt_entered_during_a_turn_goes_out_at_once_to_an_agent_that_queues() {
        use agent_client_protocol::v1::schema::StopReason;

        let mut app = with_settings();
        app.queueing = true;
        app.transcript.prompt("one".to_string());
        type_text(&mut app, "two");
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::Prompt(text) if text == "two"));
        assert_eq!(users(&app), ["one"]);
        assert_eq!(queued_texts(&app), ["two"]);
        done(&mut app, StopReason::EndTurn);
        assert_eq!(users(&app), ["one", "two"]);
        assert!(app.transcript.in_flight);
        assert_eq!(app.take_outbox(), None);
        done(&mut app, StopReason::EndTurn);
        assert!(!app.transcript.in_flight);
    }

    #[test]
    fn a_prompt_entered_during_a_turn_is_held_for_an_agent_that_does_not_queue() {
        use agent_client_protocol::v1::schema::StopReason;

        let mut app = with_settings();
        app.transcript.prompt("one".to_string());
        type_text(&mut app, "two");
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::None));
        assert_eq!(app.input, "");
        done(&mut app, StopReason::EndTurn);
        assert_eq!(app.take_outbox().as_deref(), Some("two"));
        assert_eq!(users(&app), ["one", "two"]);
    }

    fn texts(app: &App) -> Vec<&str> {
        app.transcript
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::User(message) | Entry::Agent(message) => Some(message.text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_prompt_sent_while_the_turn_is_held_goes_straight_into_the_transcript() {
        use agent_client_protocol::v1::schema::StopReason;

        let mut app = with_settings();
        app.queueing = true;
        app.transcript.prompt("one".to_string());
        app.transcript.apply(running_task("t1"));
        agent_says(&mut app, "started");
        type_text(&mut app, "two");
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::Prompt(text) if text == "two"));
        assert!(app.waiting.is_empty());
        assert_eq!(texts(&app), ["one", "started", "two"]);
        assert!(working_row(&mut app, 100).starts_with(" ◐ working · "));
        done(&mut app, StopReason::EndTurn);
        assert!(app.transcript.in_flight);
        agent_says(&mut app, "answer");
        done(&mut app, StopReason::EndTurn);
        assert!(!app.transcript.in_flight);
        assert_eq!(texts(&app), ["one", "started", "two", "answer"]);
    }

    #[test]
    fn a_prompt_sent_while_the_turn_is_held_waits_for_an_agent_that_does_not_queue() {
        use agent_client_protocol::v1::schema::StopReason;

        let mut app = with_settings();
        app.transcript.prompt("one".to_string());
        app.transcript.apply(running_task("t1"));
        agent_says(&mut app, "started");
        type_text(&mut app, "two");
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::None));
        assert_eq!(queued_texts(&app), ["two"]);
        done(&mut app, StopReason::EndTurn);
        assert_eq!(app.take_outbox().as_deref(), Some("two"));
        assert_eq!(texts(&app), ["one", "started", "two"]);
    }

    #[test]
    fn esc_puts_the_waiting_prompts_back_in_the_box_and_skips_their_replies() {
        use agent_client_protocol::v1::schema::StopReason;

        let mut app = with_settings();
        app.queueing = true;
        app.transcript.prompt("one".to_string());
        for text in ["two", "three"] {
            type_text(&mut app, text);
            press(&mut app, KeyCode::Enter);
        }
        type_text(&mut app, "four");
        assert!(matches!(press(&mut app, KeyCode::Esc), Action::None));
        assert!(matches!(press(&mut app, KeyCode::Esc), Action::Cancel));
        assert_eq!(app.input, "two\n\nthree\n\nfour");
        assert!(app.waiting.is_empty());
        for _ in 0..3 {
            done(&mut app, StopReason::Cancelled);
        }
        assert_eq!(users(&app), ["one"]);
        assert!(!app.transcript.in_flight);
    }

    #[test]
    fn waiting_prompts_show_above_the_working_line_and_count_the_rest() {
        let mut app = with_settings();
        app.transcript.prompt("one".to_string());
        app.waiting = ["two\nmore", "three", "four", "five"]
            .map(|text| Queued {
                text: text.to_string(),
                marks: Vec::new(),
            })
            .into();
        let rows = header_rows_all(&mut app, 60);
        let rows: Vec<&str> = rows.lines().collect();
        assert!(rows[19].starts_with(" queued  two "), "{}", rows[19]);
        assert!(rows[20].starts_with(" queued  three "), "{}", rows[20]);
        assert!(rows[21].starts_with(" + 2 more queued"), "{}", rows[21]);
        assert!(rows[22].starts_with(" ◐ working"), "{}", rows[22]);
    }

    fn queued_texts(app: &App) -> Vec<&str> {
        app.waiting
            .iter()
            .map(|queued| queued.text.as_str())
            .collect()
    }

    fn ask_form(app: &mut App) -> oneshot::Receiver<ElicitationAction> {
        let scenario =
            crate::scenario::Scenario::parse(include_str!("../scenarios/questions.json")).unwrap();
        let crate::scenario::Step::Elicitation(asked) = &scenario.acts[0].steps[1] else {
            panic!("the second step asks the questions");
        };
        let (reply, answer) = oneshot::channel();
        app.event(Event::Form {
            message: asked.message.clone(),
            schema: asked.requested_schema.clone(),
            reply,
        });
        answer
    }

    #[test]
    fn a_form_takes_the_keys_and_esc_sends_decline() {
        let mut app = with_settings();
        app.transcript.prompt("hi".to_string());
        let mut answer = ask_form(&mut app);
        let screen = header_rows_all(&mut app, 100);
        assert!(
            screen.contains("○ Disk    kept under the state directory"),
            "{screen}"
        );
        type_text(&mut app, "x");
        assert_eq!(app.input, "");
        press(&mut app, KeyCode::Esc);
        assert!(app.form.is_none());
        assert!(matches!(answer.try_recv(), Ok(ElicitationAction::Decline)));
    }

    #[test]
    fn a_form_takes_the_prompts_place_up_to_half_the_screen_under_the_working_line() {
        let mut app = with_settings();
        app.transcript.prompt("hi".to_string());
        ask_form(&mut app);
        ask_form(&mut app);
        let screen = header_rows_all(&mut app, 100);
        let rows: Vec<&str> = screen.lines().collect();
        assert!(rows[8].starts_with(" waiting for you"), "{screen}");
        assert!(
            rows[9].starts_with("╭──") && rows[9].ends_with("─╮"),
            "{screen}"
        );
        assert!(
            rows[10].starts_with("│ Please answer the following questions. "),
            "{screen}"
        );
        assert!(rows[12].starts_with("│   Storage · 1 of 2 "), "{screen}");
        assert!(rows[24].starts_with("╰ 1 more ─"), "{screen}");
        assert!(rows[24].ends_with(" esc dismiss ╯"), "{screen}");
        assert!(rows[25].starts_with("━━━━"), "{screen}");
        assert!(rows[..8].iter().all(|row| !row.contains("Storage")));
    }

    #[test]
    fn the_focused_options_preview_shows_in_a_box_under_its_question() {
        let mut app = with_settings();
        app.transcript.prompt("hi".to_string());
        ask_form(&mut app);
        assert!(!header_rows_all(&mut app, 100).contains("index.db"));
        press(&mut app, KeyCode::Down);
        let screen = header_rows_all(&mut app, 100);
        let rows: Vec<&str> = screen.lines().collect();
        let disk = rows.iter().position(|row| row.contains("○ Disk")).unwrap();
        assert!(rows[disk + 1].starts_with("│   ╭──"), "{screen}");
        assert!(rows[disk + 2].starts_with("│   │ state/ "), "{screen}");
        assert!(
            rows[disk + 4].starts_with("│   │     index.db │"),
            "{screen}"
        );
        press(&mut app, KeyCode::Down);
        assert!(!header_rows_all(&mut app, 100).contains("index.db"));
    }

    #[test]
    fn long_form_text_and_typed_text_wrap_inside_the_box() {
        let mut app = with_settings();
        app.transcript.prompt("hi".to_string());
        ask_form(&mut app);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Up);
        type_text(&mut app, "keep it on the fast disk and evict old blobs");
        let screen = header_rows_all(&mut app, 40);
        let rows: Vec<&str> = screen.lines().collect();
        let form = &rows[9..25];
        assert!(
            form.iter()
                .all(|row| row.starts_with('│') || row.starts_with('╭') || row.starts_with('╰')),
            "{screen}"
        );
        assert!(form.iter().all(|row| !row.contains('…')), "{screen}");
        let disk = form.iter().position(|row| row.contains("● Disk")).unwrap();
        assert!(
            form[disk].starts_with("│   ● Disk    kept under the state "),
            "{screen}"
        );
        assert!(
            form[disk + 1].starts_with("│             directory "),
            "{screen}"
        );
        assert!(
            form[disk + 2].starts_with("│ ▌ › keep it on the fast disk and "),
            "{screen}"
        );
        assert!(
            form[disk + 3].starts_with("│ ▌   evict old blobs"),
            "{screen}"
        );
    }

    #[test]
    fn the_end_of_the_turn_cancels_an_open_form() {
        use agent_client_protocol::v1::schema::StopReason;

        let mut app = with_settings();
        app.transcript.prompt("hi".to_string());
        let mut first = ask_form(&mut app);
        let mut second = ask_form(&mut app);
        assert_eq!(app.forms.len(), 1);
        done(&mut app, StopReason::EndTurn);
        assert!(app.form.is_none() && app.forms.is_empty());
        assert!(matches!(first.try_recv(), Ok(ElicitationAction::Cancel)));
        assert!(matches!(second.try_recv(), Ok(ElicitationAction::Cancel)));
    }

    #[test]
    fn a_waiting_permission_turns_the_working_line_to_waiting_for_you() {
        let mut app = with_settings();
        app.transcript.prompt("hi".to_string());
        open_permission(&mut app);
        assert!(working_row(&mut app, 100).starts_with(" waiting for you · 0s "));
    }

    #[test]
    fn the_working_line_goes_when_the_turn_ends() {
        let mut app = with_settings();
        app.transcript.prompt("hi".to_string());
        app.transcript
            .finished(agent_client_protocol::v1::schema::StopReason::EndTurn);
        assert!(!header_rows_all(&mut app, 100).contains("working"));
        assert!(status_row(&mut app, 100).ends_with("idle "));
    }

    #[test]
    fn a_picker_box_opens_above_the_status_line_over_the_value_clicked() {
        let mut app = with_settings();
        header(&mut app, 120);
        let model = app.chips[1].area;
        click(&mut app, model.x, model.y);
        header(&mut app, 120);
        let menu = app.menu_area.expect("the box is drawn");
        assert_eq!(menu.bottom(), STATUS_ROW);
        assert_eq!(menu.x, model.x);
    }

    /// A press and a release at one place, and the first action of the two
    /// that does something.
    fn click(app: &mut App, column: u16, row: u16) -> Action {
        let event = |kind| wheel(kind, KeyModifiers::NONE, column, row);
        match app.mouse(event(MouseEventKind::Down(MouseButton::Left))) {
            Action::None => app.mouse(event(MouseEventKind::Up(MouseButton::Left))),
            action => action,
        }
    }

    #[test]
    fn a_click_on_a_select_chip_opens_its_menu_and_enter_applies_the_highlighted_choice() {
        let mut app = with_settings();
        header(&mut app, 140);
        let model = app.chips[1].area;
        assert!(matches!(
            click(&mut app, model.x + 1, model.y),
            Action::None
        ));
        assert_eq!(app.menu_rows(), 3);
        let enter = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        app.key(enter, Instant::now());
        let action = app.key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            Instant::now(),
        );
        let Action::Change(Change::Config { id, value }) = action else {
            panic!("expected a config change");
        };
        assert_eq!(id, "model");
        assert_eq!(value.as_value_id().map(|id| &*id.0), Some("sonnet"));
    }

    #[test]
    fn a_click_on_a_boolean_chip_flips_it_without_a_menu() {
        let mut app = with_settings();
        header(&mut app, 140);
        let fast = app.chips[3].area;
        let action = click(&mut app, fast.x, fast.y);
        let Action::Change(Change::Config { id, value }) = action else {
            panic!("expected a config change");
        };
        assert_eq!((id.as_str(), value.as_bool()), ("fast", Some(true)));
        assert!(app.menu.is_none());
    }

    #[test]
    fn escape_and_a_click_elsewhere_close_the_menu() {
        let mut app = with_settings();
        header(&mut app, 140);
        let mode = app.chips[0].area;
        click(&mut app, mode.x, mode.y);
        assert!(app.menu.is_some());
        app.key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            Instant::now(),
        );
        assert!(app.menu.is_none());
        click(&mut app, mode.x, mode.y);
        click(&mut app, 100, 20);
        assert!(app.menu.is_none());
    }

    #[test]
    fn an_error_from_the_agent_shows_in_the_open_menu() {
        let mut app = with_settings();
        header(&mut app, 140);
        let mode = app.chips[0].area;
        click(&mut app, mode.x, mode.y);
        let Some(MenuTarget::Setting(id)) = app.menu.as_ref().map(|menu| &menu.target) else {
            panic!("the chip opened no setting menu");
        };
        let id = id.clone();
        app.event(Event::Setting {
            id,
            result: Err(Error::invalid_params()),
        });
        assert!(app.menu.as_ref().unwrap().error.is_some());
    }

    #[test]
    fn a_reply_for_another_setting_leaves_the_open_menu_alone() {
        let mut app = with_settings();
        header(&mut app, 140);
        let mode = app.chips[0].area;
        click(&mut app, mode.x, mode.y);
        app.event(Event::Setting {
            id: "elsewhere".to_string(),
            result: Err(Error::invalid_params()),
        });
        assert!(app.menu.as_ref().unwrap().error.is_none());
        assert!(matches!(
            app.transcript.entries.last(),
            Some(Entry::Notice(text)) if text.starts_with("setting failed")
        ));
        app.event(Event::Setting {
            id: "elsewhere".to_string(),
            result: Ok(Applied::Mode("x".to_string())),
        });
        assert!(app.menu.is_some());
    }

    #[test]
    fn the_overflow_chip_lists_the_settings_that_did_not_fit() {
        let mut app = with_settings();
        header(&mut app, 30);
        let overflow = app.chips.last().unwrap().area;
        click(&mut app, overflow.x, overflow.y);
        assert!(app.menu_rows() >= 2);
    }

    #[test]
    fn a_click_on_a_subagent_block_unfolds_it_and_a_second_click_folds_it() {
        let mut app = app();
        app.wide.clear();
        app.folds = vec![ui::FoldArea {
            area: Rect::new(0, 4, 60, 2),
            whole: Rect::new(0, 4, 60, 2),
            id: ToolCallId::new("t1"),
        }];
        click(&mut app, 3, 5);
        assert!(app.expanded.contains(&ToolCallId::new("t1")));
        click(&mut app, 3, 4);
        assert!(app.expanded.is_empty());
        click(&mut app, 3, 9);
        assert!(app.expanded.is_empty());
    }

    fn enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    }

    fn down() -> KeyEvent {
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)
    }

    /// An app past its first turn with the model menu open on `Opus`.
    fn in_a_conversation_with_the_model_menu_open() -> App {
        let mut app = with_settings();
        app.transcript.prompt("hello".to_string());
        app.transcript
            .finished(agent_client_protocol::v1::schema::StopReason::EndTurn);
        header(&mut app, 140);
        let model = app.chips[1].area;
        click(&mut app, model.x, model.y);
        assert!(app.menu.is_some());
        app
    }

    #[test]
    fn choosing_another_model_in_a_conversation_asks_for_the_same_choice_again() {
        let mut app = in_a_conversation_with_the_model_menu_open();
        app.key(down(), Instant::now());
        assert!(matches!(app.key(enter(), Instant::now()), Action::None));
        assert_eq!(
            app.menu.as_ref().unwrap().confirm.as_deref(),
            Some("sonnet")
        );
        let action = app.key(enter(), Instant::now());
        let Action::Change(Change::Config { id, value }) = action else {
            panic!("expected a config change");
        };
        assert_eq!(id, "model");
        assert_eq!(value.as_value_id().map(|id| &*id.0), Some("sonnet"));
    }

    #[test]
    fn choosing_a_different_model_than_the_one_waiting_moves_the_warning() {
        let mut app = in_a_conversation_with_the_model_menu_open();
        app.key(down(), Instant::now());
        app.key(enter(), Instant::now());
        app.key(down(), Instant::now());
        assert!(matches!(app.key(enter(), Instant::now()), Action::None));
        assert_eq!(app.menu.as_ref().unwrap().confirm.as_deref(), Some("haiku"));
    }

    #[test]
    fn choosing_the_current_model_or_escape_backs_out_of_the_warning() {
        let mut app = in_a_conversation_with_the_model_menu_open();
        app.key(down(), Instant::now());
        app.key(enter(), Instant::now());
        app.key(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            Instant::now(),
        );
        assert!(matches!(app.key(enter(), Instant::now()), Action::None));
        assert!(app.menu.is_none());

        let mut app = in_a_conversation_with_the_model_menu_open();
        app.key(down(), Instant::now());
        app.key(enter(), Instant::now());
        app.key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            Instant::now(),
        );
        assert!(app.menu.is_none());
    }

    #[test]
    fn a_model_choice_before_the_first_turn_applies_at_once() {
        let mut app = with_settings();
        header(&mut app, 140);
        let model = app.chips[1].area;
        click(&mut app, model.x, model.y);
        app.key(down(), Instant::now());
        assert!(matches!(
            app.key(enter(), Instant::now()),
            Action::Change(_)
        ));
    }

    #[test]
    fn other_settings_switch_at_once_in_a_conversation() {
        let mut app = with_settings();
        app.transcript.prompt("hello".to_string());
        app.transcript
            .finished(agent_client_protocol::v1::schema::StopReason::EndTurn);
        header(&mut app, 140);
        let effort = app.chips[2].area;
        click(&mut app, effort.x, effort.y);
        app.key(down(), Instant::now());
        assert!(matches!(
            app.key(enter(), Instant::now()),
            Action::Change(_)
        ));
    }

    #[test]
    fn the_model_chip_does_nothing_while_the_agent_is_working() {
        let mut app = with_settings();
        app.transcript.prompt("hello".to_string());
        header(&mut app, 140);
        let model = app.chips[1].area;
        assert!(matches!(click(&mut app, model.x, model.y), Action::None));
        assert!(app.menu.is_none());
        let mode = app.chips[0].area;
        click(&mut app, mode.x, mode.y);
        assert!(app.menu.is_some());
    }

    #[test]
    fn the_warning_shows_under_the_choices_with_the_context_size() {
        use agent_client_protocol::v1::schema::{SessionUpdate, UsageUpdate};

        let mut app = in_a_conversation_with_the_model_menu_open();
        app.settings
            .apply(&SessionUpdate::UsageUpdate(UsageUpdate::new(
                412_000, 1_000_000,
            )));
        app.key(down(), Instant::now());
        app.key(enter(), Instant::now());
        let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let screen: String = (0..30)
            .map(|y| {
                (0..140)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            screen.contains("rereads the conversation (412k tokens) through"),
            "{screen}"
        );
    }

    fn with_commands() -> App {
        use agent_client_protocol::v1::schema::{AvailableCommandsUpdate, SessionUpdate};

        let scenario = Scenario::parse(include_str!("../scenarios/resume.json")).unwrap();
        let mut app = with_settings();
        app.settings.apply(&SessionUpdate::AvailableCommandsUpdate(
            AvailableCommandsUpdate::new(scenario.commands),
        ));
        app
    }

    fn type_text(app: &mut App, text: &str) {
        for ch in text.chars() {
            app.key(
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                Instant::now(),
            );
        }
    }

    fn press(app: &mut App, code: KeyCode) -> Action {
        app.key(KeyEvent::new(code, KeyModifiers::NONE), Instant::now())
    }

    fn with_templates() -> App {
        let mut app = app();
        let template = |toml: &str| -> Template { toml::from_str(toml).unwrap() };
        app.templates = BTreeMap::from([
            (
                "away".to_string(),
                template(
                    r#"
                    text = "{tasks}\n\nI'm out for {time}. Any questions?"
                    [fields.tasks]
                    kind = "list"
                    item = "- {}"
                    [fields.time]
                    kind = "choice"
                    options = ["an hour", "the night"]
                    other = true
                    "#,
                ),
            ),
            (
                "bookkeeping".to_string(),
                template(r#"text = "Do commits, cleanups, and foam bookkeeping""#),
            ),
            ("check".to_string(), template("text = \"check {file}\"")),
        ]);
        app
    }

    #[test]
    fn slash_t_and_enter_opens_the_template_box_and_enter_in_it_opens_the_form() {
        let mut app = with_templates();
        type_text(&mut app, "/t");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(
            app.menu.as_ref().map(|menu| &menu.target),
            Some(MenuTarget::Templates)
        ));
        assert!(app.input.is_empty());
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::None));
        assert!(app.menu.is_none());
        let open = app.template_form.as_ref().unwrap();
        assert_eq!(open.name, "away");
        assert_eq!(open.form.cursor, 0);
        assert!(app.input.is_empty());
    }

    #[test]
    fn slash_t_with_a_space_and_no_name_also_opens_the_template_box() {
        let mut app = with_templates();
        type_text(&mut app, "/t ");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(
            app.menu.as_ref().map(|menu| &menu.target),
            Some(MenuTarget::Templates)
        ));
    }

    #[test]
    fn enter_on_a_complete_template_name_opens_its_form_at_once() {
        let mut app = with_templates();
        type_text(&mut app, "/t away");
        press(&mut app, KeyCode::Enter);
        assert!(app.template_form.is_some());
        assert!(app.input.is_empty());
    }

    #[test]
    fn tab_completes_a_template_name_and_cycles_through_the_matches() {
        let mut app = with_templates();
        type_text(&mut app, "/t ");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input, "/t away");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input, "/t bookkeeping");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input, "/t check");
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.input, "/t bookkeeping");
    }

    #[test]
    fn the_form_fills_the_prompt_from_its_fields_with_the_cursor_at_the_end() {
        let mut app = with_templates();
        type_text(&mut app, "/t away");
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "fix it");
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "write notes");
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Enter);
        assert!(app.template_form.is_some());
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::None));
        assert!(app.template_form.is_none());
        assert_eq!(
            app.input,
            "- fix it\n- write notes\n\nI'm out for an hour. Any questions?"
        );
        assert_eq!(app.cursor, app.input.len());
    }

    #[test]
    fn a_typed_other_answer_takes_the_place_of_the_pick() {
        let mut app = with_templates();
        type_text(&mut app, "/t away");
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "fix it");
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        type_text(&mut app, "a while");
        ctrl_enter(&mut app);
        assert_eq!(app.input, "- fix it\n\nI'm out for a while. Any questions?");
    }

    fn ctrl_enter(app: &mut App) -> Action {
        app.key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
            Instant::now(),
        )
    }

    #[test]
    fn a_field_without_an_answer_keeps_the_form_open_on_it() {
        let mut app = with_templates();
        type_text(&mut app, "/t away");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Tab);
        assert!(matches!(ctrl_enter(&mut app), Action::None));
        let open = app.template_form.as_ref().unwrap();
        assert_eq!(open.form.error.as_deref(), Some("tasks needs an answer"));
        assert_eq!(open.form.cursor, 0);
        assert!(app.input.is_empty());
    }

    #[test]
    fn esc_closes_the_form_and_leaves_the_prompt_empty() {
        let mut app = with_templates();
        type_text(&mut app, "/t away");
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "fix it");
        press(&mut app, KeyCode::Esc);
        assert!(app.template_form.is_none());
        assert!(app.input.is_empty());
    }

    #[test]
    fn ctrl_p_and_the_preview_button_show_and_hide_the_preview() {
        let mut app = with_templates();
        type_text(&mut app, "/t away");
        press(&mut app, KeyCode::Enter);
        let preview = |app: &App| app.template_form.as_ref().unwrap().preview;
        assert!(!preview(&app));
        app.key(ctrl('p'), Instant::now());
        assert!(preview(&app));
        app.preview_button = Some(Rect::new(1, 20, 14, 1));
        let click = |column| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row: 20,
            modifiers: KeyModifiers::NONE,
        };
        app.mouse(click(30));
        assert!(preview(&app));
        app.mouse(click(5));
        assert!(!preview(&app));
    }

    #[test]
    fn a_paste_goes_into_the_field_the_cursor_is_on() {
        let mut app = with_templates();
        type_text(&mut app, "/t away");
        press(&mut app, KeyCode::Enter);
        app.paste("one\ntwo");
        ctrl_enter(&mut app);
        let open = app.template_form.as_ref().unwrap();
        assert_eq!(open.form.error.as_deref(), Some("time needs an answer"));
        assert!(matches!(
            &open.form.fields[0].kind,
            crate::form::Kind::Text { text, .. } if text == "one\ntwo"
        ));
    }

    #[test]
    fn a_template_without_blanks_fills_the_prompt_and_waits_for_enter() {
        let mut app = with_templates();
        type_text(&mut app, "/t bookkeeping");
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::None));
        assert_eq!(app.input, "Do commits, cleanups, and foam bookkeeping");
        assert!(app.template_form.is_none());
        let sent = press(&mut app, KeyCode::Enter);
        assert!(
            matches!(sent, Action::Prompt(text) if text == "Do commits, cleanups, and foam bookkeeping")
        );
    }

    #[test]
    fn finishing_a_form_with_enter_or_ctrl_enter_leaves_the_text_unsent_until_enter() {
        for ctrl in [false, true] {
            let mut app = with_templates();
            type_text(&mut app, "/t check");
            press(&mut app, KeyCode::Enter);
            type_text(&mut app, "the bar");
            let modifiers = if ctrl {
                KeyModifiers::CONTROL
            } else {
                KeyModifiers::NONE
            };
            let finished = app.key(KeyEvent::new(KeyCode::Enter, modifiers), Instant::now());
            assert!(matches!(finished, Action::None));
            assert!(app.template_form.is_none());
            assert_eq!(app.input, "check the bar");
            let sent = press(&mut app, KeyCode::Enter);
            assert!(matches!(sent, Action::Prompt(text) if text == "check the bar"));
        }
    }

    #[test]
    fn slash_t_with_a_name_no_template_has_says_so_in_a_notice() {
        let mut app = with_templates();
        type_text(&mut app, "/t nope");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.unknown.as_deref(), Some("no template nope"));
    }

    #[test]
    fn a_slash_prefix_lists_the_coax_commands_that_start_with_it() {
        let mut app = with_commands();
        type_text(&mut app, "/mo");
        let names: Vec<String> = app.popup().into_iter().map(|entry| entry.fill).collect();
        assert_eq!(names, ["/mode", "/model"]);
        type_text(&mut app, "dex");
        assert!(app.popup().is_empty());
    }

    #[test]
    fn enter_on_several_matches_waits_and_on_a_complete_name_runs_it_at_once() {
        let mut app = with_commands();
        type_text(&mut app, "/mo");
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::None));
        assert_eq!(app.input, "/mo");
        assert!(app.menu.is_none());
        type_text(&mut app, "de");
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::None));
        assert!(matches!(
            &app.menu.as_ref().unwrap().target,
            MenuTarget::Setting(id) if id == "mode"
        ));
        assert!(app.input.is_empty());
    }

    #[test]
    fn tab_fills_the_first_match_cycles_on_and_shift_tab_goes_back() {
        let mut app = with_commands();
        type_text(&mut app, "/mo");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input, "/mode");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input, "/model");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input, "/mode");
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.input, "/model");
        type_text(&mut app, "x");
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input, "/model");
    }

    #[test]
    fn a_double_slash_prefix_with_one_match_goes_straight_to_the_confirm() {
        let mut app = with_commands();
        type_text(&mut app, "//comp");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.confirm.as_ref().unwrap().text(), "/compact");
    }

    #[test]
    fn the_arrows_pick_an_entry_and_enter_runs_it() {
        let mut app = with_commands();
        type_text(&mut app, "/mo");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(matches!(
            &app.menu.as_ref().unwrap().target,
            MenuTarget::Setting(id) if id == "model"
        ));
    }

    #[test]
    fn a_slash_command_never_sends_a_prompt() {
        for text in ["/mode", "/model", "/mo", "/", "/nothing", "/fast"] {
            let mut app = with_commands();
            type_text(&mut app, text);
            for _ in 0..3 {
                let action = press(&mut app, KeyCode::Enter);
                assert!(!matches!(action, Action::Prompt(_)), "{text}");
            }
        }
    }

    #[test]
    fn an_unknown_slash_command_does_nothing_and_keeps_the_text() {
        let mut app = with_commands();
        type_text(&mut app, "/nothing");
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::None));
        assert_eq!(app.input, "/nothing");
        assert!(app.menu.is_none());
    }

    #[test]
    fn escape_closes_the_list_without_cancelling_the_turn() {
        let mut app = with_commands();
        app.transcript.in_flight = true;
        type_text(&mut app, "/m");
        assert!(!app.popup().is_empty());
        assert!(matches!(press(&mut app, KeyCode::Esc), Action::None));
        assert!(app.popup().is_empty());
        assert!(matches!(press(&mut app, KeyCode::Esc), Action::None));
        assert!(matches!(press(&mut app, KeyCode::Esc), Action::Cancel));
    }

    #[test]
    fn the_model_command_does_nothing_while_the_agent_works() {
        let mut app = with_commands();
        app.transcript.in_flight = true;
        type_text(&mut app, "/model");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Enter);
        assert!(app.menu.is_none());
    }

    #[test]
    fn a_double_slash_command_asks_before_sending_and_enter_sends_it_as_a_prompt() {
        let mut app = with_commands();
        type_text(&mut app, "//compact");
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::None));
        let confirm = app.confirm.as_ref().unwrap();
        assert_eq!(confirm.text(), "/compact");
        assert_eq!(
            confirm.description,
            "Summarise the conversation to free context"
        );
        let Action::Prompt(text) = press(&mut app, KeyCode::Enter) else {
            panic!("expected a prompt");
        };
        assert_eq!(text, "/compact");
        assert!(app.confirm.is_none());
        assert!(app.input.is_empty());
    }

    #[test]
    fn the_input_after_an_agent_commands_name_goes_into_the_prompt_it_sends() {
        let mut app = with_commands();
        type_text(&mut app, "//review the parser");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.confirm.as_ref().unwrap().text(), "/review the parser");
        let Action::Prompt(text) = press(&mut app, KeyCode::Enter) else {
            panic!("expected a prompt");
        };
        assert_eq!(text, "/review the parser");
    }

    #[test]
    fn escape_backs_out_of_the_confirm_and_keeps_the_text() {
        let mut app = with_commands();
        type_text(&mut app, "//compact");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc);
        assert!(app.confirm.is_none());
        assert_eq!(app.input, "//compact");
    }

    #[test]
    fn an_agent_command_is_not_sent_while_the_agent_works() {
        let mut app = with_commands();
        app.transcript.in_flight = true;
        type_text(&mut app, "//compact");
        press(&mut app, KeyCode::Enter);
        assert!(app.confirm.is_none());
    }

    #[test]
    fn an_unknown_agent_command_is_not_sent() {
        let mut app = with_commands();
        type_text(&mut app, "//nothing");
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::None));
        assert!(app.confirm.is_none());
    }

    fn chord(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        app.key(KeyEvent::new(code, modifiers), Instant::now());
    }

    fn with_input(text: &str) -> App {
        let mut app = app();
        app.set_input(text.to_string());
        app
    }

    #[test]
    fn typing_inserts_at_the_cursor_and_moves_it_on() {
        let mut app = with_input("ac");
        chord(&mut app, KeyCode::Left, KeyModifiers::NONE);
        type_text(&mut app, "b");
        assert_eq!(app.input, "abc");
        assert_eq!(app.cursor, 2);
    }

    #[test]
    fn left_and_right_step_over_whole_multibyte_chars() {
        let mut app = with_input("a日😀");
        chord(&mut app, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(app.cursor, 4);
        chord(&mut app, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(app.cursor, 1);
        type_text(&mut app, "é");
        assert_eq!(app.input, "aé日😀");
        chord(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.cursor, 6);
        chord(&mut app, KeyCode::Right, KeyModifiers::NONE);
        chord(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.cursor, app.input.len());
    }

    #[test]
    fn backspace_and_delete_remove_whole_chars_around_the_cursor() {
        let mut app = with_input("a日b");
        chord(&mut app, KeyCode::Left, KeyModifiers::NONE);
        chord(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(app.input, "ab");
        assert_eq!(app.cursor, 1);
        chord(&mut app, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(app.input, "a");
        chord(&mut app, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(app.input, "a");
        chord(&mut app, KeyCode::Home, KeyModifiers::NONE);
        chord(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(app.input, "a");
    }

    #[test]
    fn alt_arrows_and_alt_b_f_jump_by_words() {
        let mut app = with_input("one 日本 three");
        chord(&mut app, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(app.cursor, 11);
        chord(&mut app, KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(app.cursor, 4);
        chord(&mut app, KeyCode::Right, KeyModifiers::ALT);
        assert_eq!(app.cursor, 10);
        chord(&mut app, KeyCode::Char('f'), KeyModifiers::ALT);
        assert_eq!(app.cursor, app.input.len());
        assert_eq!(app.input, "one 日本 three");
    }

    #[test]
    fn ctrl_a_and_ctrl_e_go_to_the_ends_of_the_line() {
        let mut app = with_input("日本");
        chord(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert_eq!(app.cursor, 0);
        chord(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert_eq!(app.cursor, 6);
    }

    #[test]
    fn home_and_end_go_to_the_ends_of_a_prompt_with_text() {
        let mut app = with_input("hello");
        chord(&mut app, KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(app.cursor, 0);
        chord(&mut app, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(app.cursor, 5);
    }

    #[test]
    fn end_on_an_empty_prompt_still_scrolls_the_conversation_to_the_bottom() {
        let mut app = app();
        app.scroll = 7;
        chord(&mut app, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(app.scroll, 0);
        let mut app = with_input("text");
        app.scroll = 7;
        chord(&mut app, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(app.scroll, 7);
    }

    #[test]
    fn ctrl_w_deletes_the_word_before_the_cursor() {
        let mut app = with_input("keep 日本語 tail");
        for _ in 0..5 {
            chord(&mut app, KeyCode::Left, KeyModifiers::NONE);
        }
        chord(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert_eq!(app.input, "keep  tail");
        assert_eq!(app.cursor, 5);
        chord(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert_eq!(app.input, " tail");
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn ctrl_k_deletes_from_the_cursor_to_the_end() {
        let mut app = with_input("日本語 tail");
        chord(&mut app, KeyCode::Home, KeyModifiers::NONE);
        chord(&mut app, KeyCode::Right, KeyModifiers::NONE);
        chord(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(app.input, "日");
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn ctrl_u_still_clears_the_whole_prompt_wherever_the_cursor_is() {
        let mut app = with_input("日本語");
        chord(&mut app, KeyCode::Left, KeyModifiers::NONE);
        chord(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert!(app.input.is_empty());
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn sending_a_prompt_resets_the_cursor() {
        let mut app = with_input("hi");
        chord(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.cursor, 0);
        type_text(&mut app, "x");
        assert_eq!(app.input, "x");
    }

    #[test]
    fn alt_enter_and_shift_enter_insert_a_newline_at_the_cursor_and_do_not_send() {
        let mut app = with_input("ab");
        chord(&mut app, KeyCode::Left, KeyModifiers::NONE);
        let action = app.key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
            Instant::now(),
        );
        assert!(matches!(action, Action::None));
        let action = app.key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            Instant::now(),
        );
        assert!(matches!(action, Action::None));
        assert_eq!(app.input, "a\n\nb");
        assert_eq!(app.cursor, 3);
        assert!(app.transcript.entries.is_empty());
    }

    #[test]
    fn up_and_down_move_between_lines_keeping_the_column() {
        let mut app = with_input("日本語\nabcdef\nx");
        chord(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.cursor, 10 + 1);
        chord(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.cursor, 0);
        chord(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.cursor, 0);
        chord(&mut app, KeyCode::Right, KeyModifiers::NONE);
        chord(&mut app, KeyCode::Right, KeyModifiers::NONE);
        chord(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.cursor, 10 + 4);
        chord(&mut app, KeyCode::Down, KeyModifiers::NONE);
        chord(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.cursor, app.input.len());
    }

    #[test]
    fn up_and_down_on_a_single_line_prompt_do_nothing() {
        let mut app = with_input("one line");
        chord(&mut app, KeyCode::Up, KeyModifiers::NONE);
        chord(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.cursor, 8);
        assert_eq!(app.input, "one line");
    }

    #[test]
    fn ctrl_a_ctrl_e_and_ctrl_k_work_on_the_cursors_line() {
        let mut app = with_input("one\ntwo\nthree");
        chord(&mut app, KeyCode::Up, KeyModifiers::NONE);
        chord(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert_eq!(app.cursor, 4);
        chord(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert_eq!(app.cursor, 7);
        chord(&mut app, KeyCode::Left, KeyModifiers::NONE);
        chord(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(app.input, "one\ntw\nthree");
        chord(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(app.input, "one\ntwthree");
    }

    #[test]
    fn a_paste_goes_in_at_the_cursor_with_its_newlines_and_does_not_send() {
        let mut app = with_input("ad");
        chord(&mut app, KeyCode::Left, KeyModifiers::NONE);
        app.paste("b\r\nc\ré\n");
        assert_eq!(app.input, "ab\nc\né\nd");
        assert_eq!(app.cursor, "ab\nc\né\n".len());
        assert!(app.transcript.entries.is_empty());
    }

    #[test]
    fn a_pasted_command_name_is_not_run() {
        let mut app = with_input("");
        app.paste("/mo");
        assert_eq!(app.input, "/mo");
        assert!(app.transcript.entries.is_empty());
    }

    #[test]
    fn a_paste_is_ignored_while_a_permission_box_is_open() {
        let mut app = with_input("x");
        open_permission(&mut app);
        app.paste("lost");
        assert_eq!(app.input, "x");
    }

    #[test]
    fn enter_on_a_prefix_with_one_match_runs_it_at_once() {
        let mut app = app();
        type_text(&mut app, "/ed");
        let list = app.popup();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].description, "edit the prompt in $EDITOR");
        let Action::Edit(text) = press(&mut app, KeyCode::Enter) else {
            panic!("enter on /edit should open the editor");
        };
        assert_eq!(text, "");
        assert!(app.transcript.entries.is_empty());
    }

    #[test]
    fn edit_opens_the_text_after_its_name() {
        let mut app = app();
        type_text(&mut app, "/edit two words");
        let Action::Edit(text) = press(&mut app, KeyCode::Enter) else {
            panic!("enter on /edit should open the editor");
        };
        assert_eq!(text, "two words");
        assert_eq!(app.input, "/edit two words");
    }

    #[test]
    fn ctrl_g_opens_the_whole_prompt_in_the_editor_and_leaves_it_until_the_edit_ends() {
        let mut app = app();
        type_text(&mut app, "a long\ndraft");
        press(&mut app, KeyCode::Left);
        let Action::Edit(text) = app.key(ctrl('g'), Instant::now()) else {
            panic!("ctrl+g should open the editor");
        };
        assert_eq!(text, "a long\ndraft");
        assert_eq!(app.input, "a long\ndraft");
        app.finish_edit(Ok("rewritten".to_string()));
        assert_eq!(app.input, "rewritten");
    }

    #[test]
    fn the_editors_text_replaces_the_prompt_with_the_cursor_at_the_end() {
        let mut app = with_input("/edit");
        app.finish_edit(Ok("日本\nsecond".to_string()));
        assert_eq!(app.input, "日本\nsecond");
        assert_eq!(app.cursor, app.input.len());
        assert!(app.transcript.entries.is_empty());
    }

    #[test]
    fn a_failed_editor_leaves_the_prompt_and_says_so() {
        let mut app = with_input("/edit draft");
        app.finish_edit(Err(
            "vi exited with exit status: 1, prompt unchanged".to_string()
        ));
        assert_eq!(app.input, "/edit draft");
        assert!(matches!(
            app.transcript.entries.last(),
            Some(Entry::Notice(text)) if text.contains("prompt unchanged")
        ));
    }

    #[test]
    fn log_shows_the_log_files_path_on_the_rule_and_says_so_when_there_is_none() {
        let mut app = app();
        type_text(&mut app, "/log");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.unknown.as_deref(), Some("no log file"));
        assert_eq!(app.input, "");

        let dir = std::env::temp_dir().join(format!("coax-log-command-{}", std::process::id()));
        log::open(&dir, log::Level::Info, 0, 1);
        let path = log::path().unwrap().to_path_buf();
        type_text(&mut app, "/log");
        press(&mut app, KeyCode::Enter);
        let line = format!("log: {}", path.display());
        assert_eq!(app.unknown.as_deref(), Some(line.as_str()));
        assert!(header_rows_all(&mut app, 200).contains(&line));
        assert!(app.transcript.entries.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn send(app: &mut App, text: &str) {
        app.set_input(text.to_string());
        press(app, KeyCode::Enter);
        app.transcript.in_flight = false;
    }

    fn open_history(app: &mut App) {
        type_text(app, "/history");
        press(app, KeyCode::Enter);
    }

    #[test]
    fn up_on_an_empty_prompt_opens_the_history_box_and_up_in_typed_text_does_not() {
        let mut app = app();
        press(&mut app, KeyCode::Up);
        assert!(app.menu.is_none());
        assert!(app.transcript.entries.is_empty());
        send(&mut app, "first");
        send(&mut app, "second");
        type_text(&mut app, "draft");
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Down);
        assert!(app.menu.is_none());
        assert_eq!(app.input, "draft");
        app.clear_input();
        press(&mut app, KeyCode::Up);
        assert!(matches!(
            app.menu.as_ref().map(|menu| &menu.target),
            Some(MenuTarget::History)
        ));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input, "second");
    }

    fn history_texts(app: &App) -> Vec<&str> {
        app.history.iter().map(|(text, _)| text.as_str()).collect()
    }

    #[test]
    fn a_loaded_sessions_prompts_join_the_history_oldest_first() {
        let mut app = app();
        app.transcript.prompt("from last time".to_string());
        app.transcript.in_flight = false;
        app.transcript.prompt("and after".to_string());
        app.transcript.in_flight = false;
        app.remember_loaded();
        assert_eq!(history_texts(&app), ["from last time", "and after"]);
    }

    #[test]
    fn sent_prompts_are_kept_in_order_and_a_repeat_is_kept_once() {
        let mut app = app();
        send(&mut app, "first");
        send(&mut app, "second");
        send(&mut app, "second");
        assert_eq!(history_texts(&app), ["first", "second"]);
    }

    #[test]
    fn history_is_listed_in_the_slash_popup() {
        let mut app = app();
        type_text(&mut app, "/hi");
        let list = app.popup();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].fill, "/history");
    }

    #[test]
    fn history_with_nothing_sent_says_so_and_opens_no_box() {
        let mut app = app();
        open_history(&mut app);
        assert!(app.menu.is_none());
        assert!(matches!(
            app.transcript.entries.last(),
            Some(Entry::Notice(text)) if text == "no earlier prompts"
        ));
    }

    #[test]
    fn the_history_box_lists_newest_first_and_enter_puts_the_full_text_in_the_prompt() {
        let mut app = app();
        send(&mut app, "oldest");
        send(&mut app, "middle\nwith a second line");
        send(&mut app, "newest");
        open_history(&mut app);
        assert!(matches!(
            app.menu.as_ref().map(|menu| &menu.target),
            Some(MenuTarget::History)
        ));
        assert_eq!(app.menu_rows(), 3);
        let screen = header_rows_all(&mut app, 100);
        let area = app.menu_area.expect("the box is drawn");
        let boxed: Vec<&str> = screen.lines().skip(area.y as usize).collect();
        let row = |word: &str| boxed.iter().position(|line| line.contains(word));
        assert!(row("newest") < row("middle"));
        assert!(row("middle") < row("oldest"));
        assert!(row("newest").is_some());
        assert!(!boxed.iter().any(|line| line.contains("second line")));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(app.menu.is_none());
        assert_eq!(app.input, "middle\nwith a second line");
        assert_eq!(app.cursor, app.input.len());
    }

    #[test]
    fn escape_closes_the_history_box_and_leaves_the_prompt_empty() {
        let mut app = app();
        send(&mut app, "earlier");
        open_history(&mut app);
        press(&mut app, KeyCode::Esc);
        assert!(app.menu.is_none());
        assert_eq!(app.input, "");
    }

    #[test]
    fn a_long_earlier_prompt_is_cut_to_fit_the_box() {
        let mut app = app();
        send(&mut app, &"日本語 ".repeat(40));
        open_history(&mut app);
        let screen = header_rows_all(&mut app, 100);
        assert!(screen.contains('…'));
        let area = app.menu_area.expect("the box is drawn");
        assert_eq!(area.width, 60);
    }

    #[test]
    fn the_history_box_shows_at_most_ten_rows_and_notes_a_prompt_of_several_lines() {
        let mut app = app();
        send(&mut app, "two\nlines");
        for n in 0..14 {
            send(&mut app, &format!("prompt {n}"));
        }
        open_history(&mut app);
        let screen = header_rows_all(&mut app, 100);
        assert_eq!(app.menu_area.unwrap().height, 10 + 2);
        for _ in 0..14 {
            press(&mut app, KeyCode::Down);
        }
        let screen_end = header_rows_all(&mut app, 100);
        assert!(!screen.contains("2 lines"));
        assert!(screen_end.contains("› two"), "{screen_end}");
        assert!(screen_end.contains("2 lines  "), "{screen_end}");
    }

    const NOW: Time = Time {
        year: 2026,
        month: 11,
        day: 10,
        hour: 12,
        minute: 0,
    };

    fn minutes_before_now(minutes: u8) -> Option<Time> {
        Some(Time {
            hour: NOW.hour - 1,
            minute: 60 - minutes,
            ..NOW
        })
    }

    /// The history box drawn at `NOW`, and its rows inside the border
    /// and padding, top first.
    fn history_box(app: &mut App) -> (ratatui::buffer::Buffer, Vec<Rect>) {
        open_history(app);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| ui::draw_menu(frame, app, 29, NOW))
            .unwrap();
        let area = app.menu_area.expect("the box is drawn");
        let rows = (area.y + 1..area.y + area.height - 1)
            .map(|y| Rect::new(area.x + 2, y, area.width - 4, 1))
            .collect();
        (terminal.backend().buffer().clone(), rows)
    }

    fn text_of(buffer: &ratatui::buffer::Buffer, row: Rect) -> String {
        (row.x..row.right())
            .map(|x| buffer[(x, row.y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn a_history_row_starts_with_a_dim_marker_and_ends_with_its_age() {
        let mut app = app();
        app.history = vec![
            ("older".to_string(), minutes_before_now(50)),
            ("newer".to_string(), minutes_before_now(5)),
        ];
        let (buffer, rows) = history_box(&mut app);
        let first = text_of(&buffer, rows[0]);
        assert!(first.starts_with("› newer "), "{first}");
        assert!(first.ends_with(" 5m"), "{first}");
        assert!(text_of(&buffer, rows[1]).ends_with(" 50m"));
        let marker = &buffer[(rows[0].x, rows[0].y)];
        assert!(marker.modifier.contains(ratatui::style::Modifier::DIM));
    }

    #[test]
    fn a_prompt_from_a_loaded_session_shows_no_age() {
        let mut app = app();
        app.transcript.entries.push(Entry::User(Message::new(
            "from last time".to_string(),
            None,
        )));
        app.remember_loaded();
        let (buffer, rows) = history_box(&mut app);
        assert_eq!(text_of(&buffer, rows[0]).trim_end(), "› from last time");
    }

    #[test]
    fn every_other_history_row_is_shaded_and_the_selected_row_keeps_the_selection() {
        let mut app = app();
        app.history = (0..4)
            .map(|n| (format!("prompt {n}"), minutes_before_now(5)))
            .collect();
        let (buffer, rows) = history_box(&mut app);
        let fills: Vec<_> = rows
            .iter()
            .map(|row| {
                let edges = [
                    buffer[(row.x, row.y)].bg,
                    buffer[(row.right() - 1, row.y)].bg,
                ];
                assert_eq!(edges[0], edges[1]);
                edges[0]
            })
            .collect();
        let palette = crate::palette::get();
        assert_eq!(fills[0], palette.selection);
        assert_eq!(fills[1], palette.tint);
        assert_ne!(fills[2], palette.tint);
        assert_eq!(fills[3], palette.tint);
    }

    #[test]
    fn the_line_count_of_a_long_prompt_sits_in_the_right_column_before_its_age() {
        let mut app = app();
        app.history = vec![
            ("two\nlines".to_string(), minutes_before_now(5)),
            (
                "a single line wide enough to widen the box".to_string(),
                None,
            ),
        ];
        let (buffer, rows) = history_box(&mut app);
        let row = text_of(&buffer, rows[1]);
        assert!(row.starts_with("› two     "), "{row}");
        assert!(row.ends_with(" 2 lines  5m"), "{row}");
    }

    #[test]
    fn a_history_row_wider_than_the_box_is_cut_at_sixty_columns() {
        let mut app = app();
        let long = "a".repeat(80);
        send(&mut app, &long);
        open_history(&mut app);
        let screen = header_rows_all(&mut app, 120);
        let area = app.menu_area.unwrap();
        assert_eq!(area.width, 60);
        let row = screen.lines().nth(area.y as usize + 1).unwrap();
        assert!(row.contains('…') && !row.contains(&long), "{row}");
    }

    #[test]
    fn an_exited_agent_cannot_be_sent_a_prompt_and_the_rule_says_so() {
        let mut app = with_settings();
        app.event(Event::Exited(
            "Process exited with exit status: 3: boom".to_string(),
        ));
        type_text(&mut app, "hello");
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::None));
        assert_eq!(app.input, "hello");
        let screen = header_rows_all(&mut app, 100);
        assert!(
            screen.contains("agent exited: exit status: 3: boom"),
            "{screen}"
        );
        assert!(screen.contains("the agent has exited"), "{screen}");
    }

    #[test]
    fn an_exit_closes_the_permission_box_and_ctrl_c_twice_still_quits() {
        let mut app = with_settings();
        open_permission(&mut app);
        app.event(Event::Exited("gone".to_string()));
        assert!(app.permission.is_none());
        let start = Instant::now();
        app.key(ctrl('c'), start);
        assert!(matches!(app.key(ctrl('c'), start), Action::Quit));
    }

    #[test]
    fn an_exit_while_the_agent_works_ends_the_turn_and_cancels_the_running_call() {
        use agent_client_protocol::v1::schema::{SessionUpdate, ToolCall, ToolCallStatus};

        let mut app = with_settings();
        app.transcript.prompt("go".to_string());
        app.event(Event::Update(Box::new(SessionUpdate::ToolCall(
            ToolCall::new("c1", "Read it").status(ToolCallStatus::InProgress),
        ))));
        app.event(Event::Exited("gone".to_string()));
        assert!(!app.transcript.in_flight);
        let screen = header_rows_all(&mut app, 100);
        assert!(screen.contains("cancelled"), "{screen}");
        assert!(!screen.contains("running"), "{screen}");
    }

    #[test]
    fn the_waiting_screen_shows_its_message_in_the_middle() {
        let mut terminal = Terminal::new(TestBackend::new(40, 11)).unwrap();
        terminal
            .draw(|frame| ui::draw_waiting(frame, "starting claude…"))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let row: String = (0..40)
            .map(|x| buffer[(x, 5)].symbol().to_string())
            .collect();
        assert!(row.contains("starting claude…"), "{row}");
    }

    #[test]
    fn a_transcript_that_is_shorter_one_column_narrower_never_panics_the_scroll_bar() {
        use agent_client_protocol::v1::schema::{ToolCall, ToolCallLocation, ToolKind};

        let mut app = with_settings();
        for n in 0..6 {
            let call = ToolCall::new(format!("c{n}"), format!("Read file {n}"))
                .kind(ToolKind::Read)
                .status(agent_client_protocol::v1::schema::ToolCallStatus::Completed)
                .locations(vec![ToolCallLocation::new(format!(
                    "src/module_{n}/file.rs"
                ))]);
            app.transcript
                .apply(agent_client_protocol::v1::schema::SessionUpdate::ToolCall(
                    call,
                ));
        }
        for width in 12..70 {
            for height in 6..24 {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            }
        }
    }

    fn ask(app: &mut App, id: &str) -> oneshot::Receiver<RequestPermissionOutcome> {
        use agent_client_protocol::v1::schema::{PermissionOptionId, ToolCallUpdateFields};

        let (reply, answer) = oneshot::channel();
        app.event(Event::Permission {
            update: Box::new(ToolCallUpdate::new(
                id.to_string(),
                ToolCallUpdateFields::new().title(format!("Write {id}")),
            )),
            options: vec![PermissionOption::new(
                PermissionOptionId::new("yes"),
                "Allow once",
                PermissionOptionKind::AllowOnce,
            )],
            reply,
        });
        answer
    }

    #[test]
    fn a_second_permission_request_waits_behind_the_first_and_cancels_nothing() {
        let mut app = app();
        let mut first = ask(&mut app, "a");
        let mut second = ask(&mut app, "b");
        assert_eq!(app.permission.as_ref().unwrap().title, "Write a");
        assert!(first.try_recv().is_err());
        assert!(second.try_recv().is_err());
        app.key(
            KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE),
            Instant::now(),
        );
        assert!(matches!(
            first.try_recv(),
            Ok(RequestPermissionOutcome::Selected(_))
        ));
        assert_eq!(app.permission.as_ref().unwrap().title, "Write b");
        assert!(second.try_recv().is_err());
        app.key(
            KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE),
            Instant::now(),
        );
        assert!(matches!(
            second.try_recv(),
            Ok(RequestPermissionOutcome::Selected(_))
        ));
        assert!(app.permission.is_none());
    }

    #[test]
    fn the_end_of_a_turn_closes_the_open_and_queued_permission_boxes() {
        let mut app = app();
        app.transcript.prompt("go".to_string());
        let mut first = ask(&mut app, "a");
        let mut second = ask(&mut app, "b");
        app.event(Event::Done(Ok(
            agent_client_protocol::v1::schema::StopReason::EndTurn,
        )));
        assert!(app.permission.is_none());
        assert!(matches!(
            first.try_recv(),
            Ok(RequestPermissionOutcome::Cancelled)
        ));
        assert!(matches!(
            second.try_recv(),
            Ok(RequestPermissionOutcome::Cancelled)
        ));
    }

    #[test]
    fn allow_answers_every_queued_request() {
        let mut app = app();
        app.auto_allow = true;
        let mut first = ask(&mut app, "a");
        let mut second = ask(&mut app, "b");
        assert!(first.try_recv().is_ok());
        assert!(second.try_recv().is_ok());
        assert!(app.permission.is_none());
    }

    #[test]
    fn a_menu_with_a_warning_fits_a_very_short_screen() {
        let mut app = in_a_conversation_with_the_model_menu_open();
        app.key(down(), Instant::now());
        app.key(enter(), Instant::now());
        for height in 1..10 {
            let mut terminal = Terminal::new(TestBackend::new(30, height)).unwrap();
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            let area = app.menu_area.unwrap();
            assert!(area.bottom() <= height, "{area:?} on {height} rows");
        }
    }

    #[test]
    fn a_mouse_move_asks_for_a_redraw_only_when_what_is_under_the_pointer_changes() {
        let mut app = app();
        app.wide.clear();
        app.folds = vec![ui::FoldArea {
            area: Rect::new(0, 4, 60, 2),
            whole: Rect::new(0, 4, 60, 2),
            id: ToolCallId::new("t1"),
        }];
        let moved = |column, row| wheel(MouseEventKind::Moved, KeyModifiers::NONE, column, row);
        assert!(!app.pointer(moved(3, 9)).1);
        assert!(app.pointer(moved(3, 5)).1);
        assert!(!app.pointer(moved(4, 5)).1);
        assert!(app.pointer(moved(3, 9)).1);

        let wheel_down = wheel(MouseEventKind::ScrollUp, KeyModifiers::NONE, 3, 9);
        assert!(app.pointer(wheel_down).1);
    }

    /// The screen position of the first character of `needle` in the
    /// transcript as last drawn, or of its last character when `last`.
    fn text_spot(app: &App, needle: &str, last: bool) -> Position {
        use unicode_width::UnicodeWidthStr;

        let texts = &app.texts;
        let shown = texts.start..texts.start + texts.area.height as usize;
        shown
            .filter_map(|line| Some((line, texts.lines.get(line)?.as_ref()?)))
            .find_map(|(line, text)| {
                let mut at = text.text.find(needle)?;
                if last {
                    at += needle.len() - needle.chars().last()?.len_utf8();
                }
                let col = text.col + text.text[..at].width();
                Some(Position::new(
                    texts.area.x + col as u16,
                    texts.area.y + (line - texts.start) as u16,
                ))
            })
            .unwrap_or_else(|| panic!("{needle:?} is not on screen"))
    }

    /// A left drag from `from` to `to` with a draw after each step, and what
    /// its release does.
    fn drag_over(app: &mut App, from: Position, to: Position) -> Action {
        let event = |kind, at: Position| wheel(kind, KeyModifiers::NONE, at.x, at.y);
        app.mouse(event(MouseEventKind::Down(MouseButton::Left), from));
        header_buffer(app, 80);
        app.mouse(event(MouseEventKind::Drag(MouseButton::Left), to));
        header_buffer(app, 80);
        app.mouse(event(MouseEventKind::Up(MouseButton::Left), to))
    }

    #[test]
    fn an_ascii_screen_draws_messages_tools_the_context_bar_and_status_line_in_ascii_only() {
        use crate::session::Tool;
        use agent_client_protocol::v1::schema::{
            ToolCallId, ToolCallStatus, ToolKind, UsageUpdate,
        };

        let tool = |id: &str, kind: ToolKind, status: ToolCallStatus| {
            Entry::Tool(Tool {
                id: ToolCallId::new(id.to_string()),
                title: format!("tool {id}"),
                kind,
                status,
                locations: Vec::new(),
                content: Vec::new(),
                cancelled: false,
                changed: (2, 1),
                revision: 0,
                agent: false,
                children: Vec::new(),
                last_child_update: None,
            })
        };
        let mut app = with_settings();
        let usage = UsageUpdate::new(60_000, 200_000);
        app.settings
            .apply(&agent_client_protocol::v1::schema::SessionUpdate::UsageUpdate(usage));
        app.transcript.entries = vec![
            prompt("① can you look at this?"),
            Entry::Thought("a thought".to_string()),
            tool("r1", ToolKind::Read, ToolCallStatus::Completed),
            tool("e1", ToolKind::Edit, ToolCallStatus::Completed),
            tool("x1", ToolKind::Execute, ToolCallStatus::Failed),
            reply(
                "## a header\n\n- a list\n  - nested\n\n> a quote\n\n---\n\n\
                 | a | b |\n|---|---|\n| 1 | 2 |\n\n```rust\nlet number = 42;\n```",
            ),
        ];
        let mut buffer = header_buffer(&mut app, 80);
        crate::glyphs::to_ascii(&mut buffer);
        let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.is_ascii(), "{text}");
        assert!(text.contains("3 tools"), "{text}");
    }

    fn drawn_with(entries: Vec<Entry>) -> App {
        let mut app = app();
        app.wide.clear();
        app.transcript.entries = entries;
        header_buffer(&mut app, 80);
        app
    }

    fn copied_by(action: Action) -> String {
        match action {
            Action::Copy(text) => text,
            _ => panic!("expected a copy"),
        }
    }

    #[test]
    fn a_drag_across_a_wrapped_paragraph_copies_one_line_joined_with_a_space() {
        let words: Vec<String> = (1..=40).map(|n| format!("word{n}")).collect();
        let text = words.join(" ");
        let mut app = drawn_with(vec![reply(text.clone())]);
        let from = text_spot(&app, "word1 ", false);
        let to = text_spot(&app, "word40", true);
        assert!(to.y > from.y, "the paragraph wraps");
        assert_eq!(copied_by(drag_over(&mut app, from, to)), text);
    }

    #[test]
    fn a_drag_across_a_word_split_over_rows_joins_it_with_nothing() {
        let word = format!("a{}z", "x".repeat(100));
        let mut app = drawn_with(vec![reply(word.clone())]);
        let from = text_spot(&app, "ax", false);
        let to = text_spot(&app, "xz", true);
        assert!(to.y > from.y, "the word is split");
        assert_eq!(copied_by(drag_over(&mut app, from, to)), word);
    }

    #[test]
    fn a_line_break_in_a_message_stays_a_line_break_in_a_copy() {
        let mut app = drawn_with(vec![prompt("first line\nsecond line".to_string())]);
        let from = text_spot(&app, "first", false);
        let to = text_spot(&app, "second line", true);
        assert_eq!(
            copied_by(drag_over(&mut app, from, to)),
            "first line\nsecond line"
        );
    }

    #[test]
    fn a_copy_leaves_out_borders_gutters_and_a_code_blocks_header() {
        let long = "abcdefghij".repeat(10);
        let text = format!("before\n\n```\n{long}\nshort\n```\n\nafter");
        let mut app = drawn_with(vec![reply(text)]);
        let from = text_spot(&app, "before", false);
        let to = text_spot(&app, "after", true);
        let copy = copied_by(drag_over(&mut app, from, to));
        assert_eq!(copy, format!("before\n\n{long}\nshort\n\nafter"));

        let buffer = header_buffer(&mut app, 80);
        let tint = crate::palette::get().tint;
        assert_eq!(buffer[(from.x, from.y)].bg, tint);
        assert_ne!(buffer[(from.x - 2, from.y)].bg, tint);
    }

    #[test]
    fn the_selection_stays_after_the_release_and_goes_at_the_next_click() {
        let mut app = drawn_with(vec![reply("one two three".to_string())]);
        let from = text_spot(&app, "one", false);
        let to = text_spot(&app, "two", true);
        assert_eq!(copied_by(drag_over(&mut app, from, to)), "one two");
        assert!(app.selection.is_some());
        assert!(matches!(click(&mut app, from.x, from.y), Action::None));
        assert!(app.selection.is_none());
    }

    #[test]
    fn a_long_notice_with_newlines_draws_as_one_cut_line_when_the_screen_cuts_it_after_a_newline() {
        let mut app = app();
        app.transcript.notice(
            "prompt failed: Internal error: You've hit your 5-hour limit · resets \
             9:30pm (America/Denver): {\n  \"errorKind\": \"rate_limit\"\n}"
                .to_string(),
        );
        let screen = header_rows_all(&mut app, 110);
        let row = screen
            .lines()
            .find(|row| row.contains("prompt failed"))
            .expect("the notice is drawn");
        assert!(row.trim_end().ends_with('…'), "{screen}");
    }

    #[test]
    fn a_double_click_copies_the_word_and_a_triple_click_the_whole_wrapped_line() {
        let long = "first words then a line long enough to wrap over two rows of \
                    the transcript at eighty columns wide, ending here";
        let mut app = drawn_with(vec![reply(format!("alpha beta_gamma delta\n\n{long}"))]);
        let at = text_spot(&app, "beta", false);
        assert!(matches!(click(&mut app, at.x, at.y), Action::None));
        assert_eq!(copied_by(click(&mut app, at.x, at.y)), "beta_gamma");
        assert!(app.selection.is_some());
        let at = text_spot(&app, "then", false);
        click(&mut app, at.x, at.y);
        click(&mut app, at.x, at.y);
        assert_eq!(copied_by(click(&mut app, at.x, at.y)), long);
    }

    #[test]
    fn clicks_too_far_apart_in_time_select_nothing() {
        let mut app = drawn_with(vec![reply("alpha beta")]);
        let at = text_spot(&app, "beta", false);
        click(&mut app, at.x, at.y);
        if let Some((when, _, _)) = &mut app.last_press {
            *when -= MULTI_CLICK * 2;
        }
        assert!(matches!(click(&mut app, at.x, at.y), Action::None));
        assert!(app.selection.is_none());
    }

    fn mark_drag(app: &mut App, from: &str, to: &str) {
        let from = text_spot(app, from, false);
        let to = text_spot(app, to, true);
        drag_over(app, from, to);
        header_buffer(app, 80);
    }

    #[test]
    fn a_marked_passage_reaches_the_agent_quoted_above_the_question() {
        let mut app = drawn_with(vec![prompt("go"), reply("one two three")]);
        mark_drag(&mut app, "one", "two");
        let button = app.mark_button.expect("a mark button");
        assert!(matches!(click(&mut app, button.x, button.y), Action::None));
        assert_eq!(app.input, "① ");
        assert!(app.selection.is_none());
        let screen = header_rows_all(&mut app, 80);
        assert!(screen.contains("① one two three"), "{screen}");
        type_text(&mut app, "why?");
        let Action::Prompt(sent) = press(&mut app, KeyCode::Enter) else {
            panic!("the prompt should go out");
        };
        assert_eq!(sent, "[1]\n> one two\n\n[1] why?");
        assert!(app.marks.is_empty());
        let Some(Entry::User(sent)) = app.transcript.entries.last() else {
            panic!("the prompt should be in the transcript");
        };
        assert_eq!(sent.text, "① why?");
        assert_eq!(sent.marks[0].text, "one two");
        header_buffer(&mut app, 80);
        let chip = app
            .link_areas
            .iter()
            .find(|area| area.target == "mark:1")
            .expect("the sent chip can be hovered");
        assert_eq!(chip.title.as_deref(), Some("one two"));
        let area = chip.area;
        let buffer = header_buffer(&mut app, 80);
        assert_eq!(buffer[(area.x, area.y)].symbol(), "①");
    }

    /// The passage of a mark made by a drag from `from` to `to`.
    fn marked_text(app: &mut App, from: &str, to: &str) -> String {
        mark_drag(app, from, to);
        let button = app.mark_button.expect("a mark button");
        click(app, button.x, button.y);
        app.marks.last().expect("a mark").text.clone()
    }

    #[test]
    fn a_marked_passage_reaches_the_agent_as_its_markdown_source() {
        let mut app = drawn_with(vec![reply("say **one** and `two` then three")]);
        assert_eq!(marked_text(&mut app, "one", "two"), "**one** and `two`");
        type_text(&mut app, "why?");
        let Action::Prompt(sent) = press(&mut app, KeyCode::Enter) else {
            panic!("the prompt should go out");
        };
        assert_eq!(sent, "[1]\n> **one** and `two`\n\n[1] why?");
    }

    #[test]
    fn marking_exactly_the_text_of_inline_code_or_a_link_takes_its_markers() {
        let text = "see `run()` or [the docs](https://example.com/a) now";
        let mut app = drawn_with(vec![reply(text)]);
        assert_eq!(marked_text(&mut app, "run", "()"), "`run()`");
        assert_eq!(
            marked_text(&mut app, "the docs", "docs"),
            "[the docs](https://example.com/a)"
        );
    }

    #[test]
    fn a_mark_from_a_list_bullet_starts_at_the_items_text() {
        let mut app = drawn_with(vec![reply("- first item here\n- second")]);
        assert_eq!(marked_text(&mut app, "•", "here"), "first item here");
    }

    #[test]
    fn a_mark_inside_a_markdown_block_drawn_as_markdown_takes_its_source() {
        let text = "before\n\n```md\nsome **bold** words\n```";
        let mut app = drawn_with(vec![reply(text)]);
        assert_eq!(marked_text(&mut app, "bold", "words"), "**bold** words");
    }

    #[test]
    fn a_marked_passage_stays_on_its_words_when_the_reply_reflows() {
        let words: Vec<String> = (1..=40).map(|n| format!("word{n}")).collect();
        let mut app = drawn_with(vec![reply(words.join(" "))]);
        marked_text(&mut app, "word5 ", "word30");
        let buffer = header_buffer(&mut app, 50);
        let area = app.texts.area;
        let selection = crate::palette::get().selection;
        let rows: Vec<String> = (area.top()..area.bottom())
            .map(|y| {
                (area.left()..area.right())
                    .filter(|&x| buffer[(x, y)].bg == selection)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .filter(|row| !row.is_empty())
            .collect();
        assert!(rows.len() > 2, "the passage wraps over more rows at 50");
        assert_eq!(rows.join(" "), words[4..30].join(" "));
    }

    #[test]
    fn the_mark_button_turns_solid_under_the_pointer() {
        let mut app = drawn_with(vec![reply("one two three")]);
        mark_drag(&mut app, "one", "two");
        let button = app.mark_button.expect("a mark button");
        let resting = header_buffer(&mut app, 80)[(button.x + 1, button.y)].style();
        let moved = wheel(
            MouseEventKind::Moved,
            KeyModifiers::NONE,
            button.x,
            button.y,
        );
        app.mouse(moved);
        assert!(app.hover_mark);
        let hovered = header_buffer(&mut app, 80)[(button.x + 1, button.y)].style();
        assert_ne!(resting.bg, hovered.bg);
        assert!(
            hovered
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }

    #[test]
    fn backspace_over_a_chip_drops_its_mark() {
        let mut app = drawn_with(vec![reply("one two three")]);
        mark_drag(&mut app, "one", "two");
        let button = app.mark_button.expect("a mark button");
        click(&mut app, button.x, button.y);
        assert_eq!(app.marks.len(), 1);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.marks.len(), 1);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.input, "");
        assert!(app.marks.is_empty());
    }

    #[test]
    fn a_selection_that_leaves_its_reply_offers_no_mark() {
        let mut app = drawn_with(vec![reply("one two"), reply("three four")]);
        mark_drag(&mut app, "two", "three");
        assert!(app.selection.is_some());
        assert_eq!(app.mark_button, None);
    }

    #[test]
    fn a_click_on_a_call_without_a_drag_still_folds_it_and_a_drag_does_not() {
        let mut app = with_settings();
        let fold = unfolded_edit(&mut app);
        let head = Position::new(fold.area.x + 3, fold.area.y);
        let below = Position::new(fold.area.x + 8, fold.area.y + 1);
        assert!(matches!(drag_over(&mut app, head, below), Action::Copy(_)));
        assert!(app.expanded.contains(&fold.id));
        click(&mut app, head.x, head.y);
        assert!(!app.expanded.contains(&fold.id));
        assert!(app.selection.is_none());
    }

    #[test]
    fn a_drag_from_the_conversations_scroll_bar_scrolls_and_selects_nothing() {
        let notices = (0..60).map(|n| Entry::Notice(format!("notice {n}")));
        let mut app = drawn_with(notices.collect());
        let bar = app.v_bar.as_ref().expect("a scroll bar").area;
        let thumb = bar.y + bar.height - 1;
        let from = Position::new(bar.x, thumb);
        let to = Position::new(bar.x, bar.y);
        assert!(matches!(drag_over(&mut app, from, to), Action::None));
        assert!(app.scroll > 0);
        assert!(app.selection.is_none());
    }

    #[test]
    fn a_drag_across_a_panned_block_copies_its_whole_lines() {
        let long: String = (0..100).map(|n| char::from(b'a' + n % 26)).collect();
        let mut app = drawn_with(vec![reply(format!(
            "before\n\n```rust\n{long}\nshort\n```\n\nafter"
        ))]);
        app.pans.insert((0, 1), 10);
        header_buffer(&mut app, 80);
        let from = text_spot(&app, "before", false);
        let to = text_spot(&app, "after", true);
        let text = copied_by(drag_over(&mut app, from, to));
        assert_eq!(text, format!("before\n\n{long}\nshort\n\nafter"));
    }

    #[test]
    fn a_drag_held_below_the_transcript_scrolls_it_down_a_line_at_a_time() {
        let notices = (0..60).map(|n| Entry::Notice(format!("notice {n}")));
        let mut app = drawn_with(notices.collect());
        app.scroll = 5;
        header_buffer(&mut app, 80);
        let from = text_spot(&app, "notice 55", false);
        let event = |kind, at: Position| wheel(kind, KeyModifiers::NONE, at.x, at.y);
        app.mouse(event(MouseEventKind::Down(MouseButton::Left), from));
        let below = Position::new(from.x, app.texts.area.bottom());
        app.mouse(event(MouseEventKind::Drag(MouseButton::Left), below));
        assert_eq!(app.edge_scroll(), -1);
        app.scroll_past_edge();
        app.scroll_past_edge();
        assert_eq!(app.scroll, 3);
        header_buffer(&mut app, 80);
        let last = app.texts.start + app.texts.area.height as usize - 1;
        assert_eq!(app.selection.as_ref().unwrap().head.line, last);
    }
}
