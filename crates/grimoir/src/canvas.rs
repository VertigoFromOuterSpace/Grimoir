//! The reader canvas.
//!
//! This is a small custom widget instead of a stack of `GtkPicture`s so page
//! turns can be animated, double-page spreads share one coordinate system, and
//! continuous scrolling does not require building one widget per page.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::gdk::{self, Texture};
use gtk::glib;
use gtk::graphene;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

const MARGIN: f64 = 24.0;
const PAGE_GAP: f64 = 12.0;
const TURN_DURATION: f64 = 0.24;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FitMode {
    #[default]
    Page,
    Width,
    Height,
    Original,
}

impl FitMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Page => "Page",
            Self::Width => "Width",
            Self::Height => "Height",
            Self::Original => "Original",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReadingDirection {
    #[default]
    Ltr,
    Rtl,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CanvasMode {
    #[default]
    Paged,
    Continuous,
}

#[derive(Clone, Debug)]
pub struct PageImage {
    pub index: usize,
    pub texture: Option<Texture>,
    /// Page size in PDF points. Used for layout so unloaded pages still occupy
    /// the right amount of space in continuous mode.
    pub point_width: f64,
    pub point_height: f64,
}

/// A translucent rectangle on a page, in normalized page coordinates.
#[derive(Clone, Debug)]
pub struct PageMark {
    pub page: usize,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub color: gdk::RGBA,
}

impl PageImage {
    fn size(&self) -> (f64, f64) {
        if self.point_width > 0.0 && self.point_height > 0.0 {
            (self.point_width, self.point_height)
        } else {
            (600.0, 840.0)
        }
    }
}

pub(super) struct State {
    mode: CanvasMode,
    fit: FitMode,
    direction: ReadingDirection,
    zoom: f64,
    pan: (f64, f64),
    scroll: f64,
    current: Vec<PageImage>,
    previous: Vec<PageImage>,
    animation: Option<TurnAnimation>,
    animation_source: Option<glib::SourceId>,
    continuous: Vec<PageImage>,
    marks: Vec<PageMark>,
    annotating: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            mode: CanvasMode::Paged,
            fit: FitMode::Page,
            direction: ReadingDirection::Ltr,
            zoom: 1.0,
            pan: (0.0, 0.0),
            scroll: 0.0,
            current: Vec::new(),
            previous: Vec::new(),
            animation: None,
            animation_source: None,
            continuous: Vec::new(),
            marks: Vec::new(),
            annotating: false,
        }
    }
}

struct TurnAnimation {
    progress: f64,
    forward: bool,
}

struct SpreadLayout {
    rects: Vec<(f64, f64, f64, f64)>,
    content_width: f64,
    content_height: f64,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PageCanvas {
        pub(super) state: RefCell<State>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PageCanvas {
        const NAME: &'static str = "GrimoirPageCanvas";
        type Type = super::PageCanvas;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for PageCanvas {}

    impl WidgetImpl for PageCanvas {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let widget = widget.as_ref();
            let width = gtk::prelude::WidgetExt::width(widget) as f64;
            let height = gtk::prelude::WidgetExt::height(widget) as f64;
            if width <= 0.0 || height <= 0.0 {
                return;
            }

            let mut state = self.state.borrow_mut();
            if state.mode == CanvasMode::Continuous {
                draw_continuous(snapshot, &state.continuous, width, height, &state);
                return;
            }

            let Some(animation) = state.animation.as_mut() else {
                draw_spread(
                    snapshot,
                    &state.current,
                    width,
                    height,
                    state.fit,
                    state.zoom,
                    state.pan,
                    &state.marks,
                );
                return;
            };

            let progress = smoothstep(animation.progress.clamp(0.0, 1.0));
            let move_dir = match (animation.forward, state.direction) {
                (true, ReadingDirection::Ltr) => -1.0,
                (true, ReadingDirection::Rtl) => 1.0,
                (false, ReadingDirection::Ltr) => 1.0,
                (false, ReadingDirection::Rtl) => -1.0,
            };
            let slide = width * 0.07;
            let old_offset = move_dir * slide * progress;
            let new_offset = -move_dir * slide * (1.0 - progress);

            // A cross-fade needs the old page before the first pop and the new
            // page after it; the second pop closes the cross-fade.
            snapshot.push_cross_fade(progress);
            snapshot.save();
            snapshot.translate(&graphene::Point::new(old_offset as f32, 0.0));
            draw_spread(
                snapshot,
                &state.previous,
                width,
                height,
                state.fit,
                state.zoom,
                state.pan,
                &state.marks,
            );
            snapshot.restore();
            snapshot.pop();
            snapshot.save();
            snapshot.translate(&graphene::Point::new(new_offset as f32, 0.0));
            draw_spread(
                snapshot,
                &state.current,
                width,
                height,
                state.fit,
                state.zoom,
                state.pan,
                &state.marks,
            );
            snapshot.restore();
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    pub struct PageCanvas(ObjectSubclass<imp::PageCanvas>)
        @extends gtk::Widget,
        @implements gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PageCanvas {
    fn default() -> Self {
        Self::new()
    }
}

impl PageCanvas {
    pub fn new() -> Self {
        let canvas: Self = glib::Object::builder().build();
        canvas.set_hexpand(true);
        canvas.set_vexpand(true);
        canvas.add_css_class("grimoir-canvas");

        let drag_start = Rc::new(Cell::new((0.0, 0.0)));
        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        {
            let canvas = canvas.clone();
            let drag_start = drag_start.clone();
            drag.connect_drag_begin(move |_, _, _| {
                let state = canvas.imp().state.borrow();
                drag_start.set(state.pan);
            });
        }
        {
            let canvas = canvas.clone();
            drag.connect_drag_update(move |_, dx, dy| {
                if canvas.imp().state.borrow().annotating {
                    return;
                }
                if canvas.mode() == CanvasMode::Continuous {
                    canvas.scroll_by(-dy);
                    return;
                }
                let (x, y) = drag_start.get();
                canvas.set_pan(x + dx, y + dy);
            });
        }
        canvas.add_controller(drag);
        canvas
    }

    pub fn set_mode(&self, mode: CanvasMode) {
        let mut state = self.imp().state.borrow_mut();
        if state.mode == mode {
            return;
        }
        state.mode = mode;
        state.animation = None;
        state.previous.clear();
        stop_animation_source(&mut state);
        drop(state);
        self.queue_draw();
    }

    pub fn mode(&self) -> CanvasMode {
        self.imp().state.borrow().mode
    }

    pub fn set_direction(&self, direction: ReadingDirection) {
        self.imp().state.borrow_mut().direction = direction;
    }

    pub fn set_fit(&self, fit: FitMode) {
        let mut state = self.imp().state.borrow_mut();
        state.fit = fit;
        state.pan = (0.0, 0.0);
        drop(state);
        self.queue_draw();
    }

    pub fn fit(&self) -> FitMode {
        self.imp().state.borrow().fit
    }

    pub fn set_zoom(&self, zoom: f64) {
        let mut state = self.imp().state.borrow_mut();
        state.zoom = zoom.clamp(0.25, 4.0);
        drop(state);
        self.queue_draw();
    }

    pub fn zoom(&self) -> f64 {
        self.imp().state.borrow().zoom
    }

    pub fn zoom_by(&self, factor: f64) -> f64 {
        let next = (self.zoom() * factor).clamp(0.25, 4.0);
        self.set_zoom(next);
        next
    }

    pub fn reset_view(&self) {
        let mut state = self.imp().state.borrow_mut();
        state.zoom = 1.0;
        state.pan = (0.0, 0.0);
        drop(state);
        self.queue_draw();
    }

    pub fn set_pan(&self, x: f64, y: f64) {
        let mut state = self.imp().state.borrow_mut();
        state.pan = (x, y);
        clamp_pan(
            &mut state,
            gtk::prelude::WidgetExt::width(self) as f64,
            gtk::prelude::WidgetExt::height(self) as f64,
        );
        drop(state);
        self.queue_draw();
    }

    pub fn pan_by(&self, dx: f64, dy: f64) {
        let mut state = self.imp().state.borrow_mut();
        state.pan.0 += dx;
        state.pan.1 += dy;
        clamp_pan(
            &mut state,
            gtk::prelude::WidgetExt::width(self) as f64,
            gtk::prelude::WidgetExt::height(self) as f64,
        );
        drop(state);
        self.queue_draw();
    }

    pub fn pan(&self) -> (f64, f64) {
        self.imp().state.borrow().pan
    }

    pub fn set_marks(&self, marks: Vec<PageMark>) {
        self.imp().state.borrow_mut().marks = marks;
        self.queue_draw();
    }

    pub fn set_annotating(&self, annotating: bool) {
        self.imp().state.borrow_mut().annotating = annotating;
    }

    /// Returns the page under a widget-space point and the point in normalized
    /// page coordinates (0..1, origin at the top-left of the page).
    pub fn page_at(&self, x: f64, y: f64) -> Option<(usize, (f64, f64))> {
        let state = self.imp().state.borrow();
        let viewport_width = gtk::prelude::WidgetExt::width(self) as f64;
        let viewport_height = gtk::prelude::WidgetExt::height(self) as f64;
        if viewport_width <= 0.0 || viewport_height <= 0.0 {
            return None;
        }

        if state.mode == CanvasMode::Continuous {
            let display_width = (viewport_width - 2.0 * MARGIN).max(1.0) * state.zoom;
            let page_x = (viewport_width - display_width) / 2.0 + state.pan.0;
            let (_, positions) = continuous_metrics(&state.continuous, viewport_width, state.zoom);
            for (page, (top, height)) in state.continuous.iter().zip(positions.iter()) {
                let page_y = *top - state.scroll;
                if x >= page_x
                    && x <= page_x + display_width
                    && y >= page_y
                    && y <= page_y + *height
                {
                    return Some((
                        page.index,
                        (
                            ((x - page_x) / display_width).clamp(0.0, 1.0),
                            ((y - page_y) / height).clamp(0.0, 1.0),
                        ),
                    ));
                }
            }
            return None;
        }

        let layout = spread_layout(
            &state.current,
            viewport_width,
            viewport_height,
            state.fit,
            state.zoom,
            state.pan,
        );
        for (page, (rect_x, rect_y, rect_width, rect_height)) in
            state.current.iter().zip(layout.rects.iter())
        {
            if x >= *rect_x
                && x <= *rect_x + *rect_width
                && y >= *rect_y
                && y <= *rect_y + *rect_height
            {
                return Some((
                    page.index,
                    (
                        ((x - rect_x) / rect_width).clamp(0.0, 1.0),
                        ((y - rect_y) / rect_height).clamp(0.0, 1.0),
                    ),
                ));
            }
        }
        None
    }

    pub fn set_spread(&self, pages: Vec<PageImage>, animate: bool, forward: bool) {
        let mut state = self.imp().state.borrow_mut();
        let previous = std::mem::take(&mut state.current);
        state.current = pages;
        state.pan = (0.0, 0.0);
        if animate && !previous.is_empty() && !state.current.is_empty() {
            state.previous = previous;
            state.animation = Some(TurnAnimation {
                progress: 0.0,
                forward,
            });
            stop_animation_source(&mut state);
            drop(state);
            self.start_animation();
        } else {
            state.previous.clear();
            state.animation = None;
            stop_animation_source(&mut state);
            drop(state);
        }
        self.queue_draw();
    }

    /// Replaces the visible spread without resetting zoom or pan. Used when a
    /// page texture or the ink filter changes.
    pub fn refresh_spread(&self, pages: Vec<PageImage>) {
        let mut state = self.imp().state.borrow_mut();
        if state.mode == CanvasMode::Continuous {
            return;
        }
        state.current = pages;
        state.animation = None;
        state.previous.clear();
        stop_animation_source(&mut state);
        drop(state);
        self.queue_draw();
    }

    pub fn set_continuous_pages(&self, pages: Vec<PageImage>) {
        let mut state = self.imp().state.borrow_mut();
        state.continuous = pages;
        state.scroll = 0.0;
        state.animation = None;
        state.previous.clear();
        stop_animation_source(&mut state);
        drop(state);
        self.queue_draw();
    }

    pub fn update_continuous_page(&self, page: PageImage) {
        let mut state = self.imp().state.borrow_mut();
        if let Some(existing) = state
            .continuous
            .iter_mut()
            .find(|existing| existing.index == page.index)
        {
            *existing = page;
            drop(state);
            self.queue_draw();
        }
    }

    pub fn scroll_by(&self, dy: f64) {
        let mut state = self.imp().state.borrow_mut();
        let max = continuous_max_scroll(
            &state.continuous,
            gtk::prelude::WidgetExt::width(self) as f64,
            gtk::prelude::WidgetExt::height(self) as f64,
            state.zoom,
        );
        state.scroll = (state.scroll + dy).clamp(0.0, max);
        drop(state);
        self.queue_draw();
    }

    pub fn scroll_to_page(&self, index: usize) {
        let mut state = self.imp().state.borrow_mut();
        let width = gtk::prelude::WidgetExt::width(self) as f64;
        let display_width = (width - 2.0 * MARGIN).max(1.0) * state.zoom;
        let mut y = MARGIN;
        for page in &state.continuous {
            let (_, point_height) = page.size();
            let height = display_width * point_height / page.point_width.max(1.0);
            if page.index == index {
                state.scroll = (y - MARGIN).max(0.0);
                drop(state);
                self.queue_draw();
                return;
            }
            y += height + PAGE_GAP;
        }
    }

    pub fn scroll(&self) -> f64 {
        self.imp().state.borrow().scroll
    }

    pub fn visible_range(&self) -> Option<(usize, usize)> {
        let state = self.imp().state.borrow();
        if state.continuous.is_empty() {
            return None;
        }
        let width = gtk::prelude::WidgetExt::width(self) as f64;
        let height = gtk::prelude::WidgetExt::height(self) as f64;
        let display_width = (width - 2.0 * MARGIN).max(1.0) * state.zoom;
        let mut y = MARGIN - state.scroll;
        let mut first = None;
        let mut last = None;
        for page in &state.continuous {
            let (_, point_height) = page.size();
            let page_height = display_width * point_height / page.point_width.max(1.0);
            if y + page_height > 0.0 && y < height {
                first.get_or_insert(page.index);
                last = Some(page.index);
            }
            y += page_height + PAGE_GAP;
        }
        first.zip(last)
    }

    pub fn top_page(&self) -> Option<usize> {
        let state = self.imp().state.borrow();
        if state.continuous.is_empty() {
            return None;
        }
        let width = gtk::prelude::WidgetExt::width(self) as f64;
        let display_width = (width - 2.0 * MARGIN).max(1.0) * state.zoom;
        let mut y = MARGIN - state.scroll;
        for page in &state.continuous {
            let (_, point_height) = page.size();
            let page_height = display_width * point_height / page.point_width.max(1.0);
            if y + page_height > 0.0 {
                return Some(page.index);
            }
            y += page_height + PAGE_GAP;
        }
        state.continuous.last().map(|page| page.index)
    }

    pub fn continuous_is_ready(&self) -> bool {
        !self.imp().state.borrow().continuous.is_empty()
    }

    fn start_animation(&self) {
        let imp = self.imp();
        let mut state = imp.state.borrow_mut();
        stop_animation_source(&mut state);
        let weak = self.downgrade();
        let source = glib::timeout_add_local(Duration::from_millis(16), move || {
            let Some(canvas) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let imp = canvas.imp();
            let mut state = imp.state.borrow_mut();
            let Some(animation) = state.animation.as_mut() else {
                return glib::ControlFlow::Break;
            };
            animation.progress += 0.016 / TURN_DURATION;
            let done = animation.progress >= 1.0;
            if done {
                animation.progress = 1.0;
            }
            drop(state);
            canvas.queue_draw();
            if done {
                let mut state = imp.state.borrow_mut();
                state.animation = None;
                state.previous.clear();
                state.animation_source = None;
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        state.animation_source = Some(source);
    }
}

fn stop_animation_source(state: &mut State) {
    if let Some(source) = state.animation_source.take() {
        source.remove();
    }
}

fn smoothstep(value: f64) -> f64 {
    value * value * (3.0 - 2.0 * value)
}

#[allow(clippy::too_many_arguments)]
fn draw_spread(
    snapshot: &gtk::Snapshot,
    pages: &[PageImage],
    viewport_width: f64,
    viewport_height: f64,
    fit: FitMode,
    zoom: f64,
    pan: (f64, f64),
    marks: &[PageMark],
) {
    if pages.is_empty() {
        return;
    }
    let layout = spread_layout(pages, viewport_width, viewport_height, fit, zoom, pan);
    for (page, (x, y, width, height)) in pages.iter().zip(layout.rects.iter()) {
        if let Some(texture) = &page.texture {
            snapshot.append_texture(
                texture,
                &graphene::Rect::new(*x as f32, *y as f32, *width as f32, *height as f32),
            );
        }
        draw_marks(snapshot, marks, page.index, *x, *y, *width, *height);
    }
}

fn draw_marks(
    snapshot: &gtk::Snapshot,
    marks: &[PageMark],
    page_index: usize,
    page_x: f64,
    page_y: f64,
    page_width: f64,
    page_height: f64,
) {
    for mark in marks.iter().filter(|mark| mark.page == page_index) {
        let rect = graphene::Rect::new(
            (page_x + mark.x * page_width) as f32,
            (page_y + mark.y * page_height) as f32,
            (mark.width * page_width) as f32,
            (mark.height * page_height) as f32,
        );
        snapshot.append_color(&mark.color, &rect);
    }
}

fn spread_layout(
    pages: &[PageImage],
    viewport_width: f64,
    viewport_height: f64,
    fit: FitMode,
    zoom: f64,
    pan: (f64, f64),
) -> SpreadLayout {
    let count = pages.len().max(1);
    let total_gap = PAGE_GAP * (count.saturating_sub(1)) as f64;
    let mut natural_width = 0.0;
    let mut natural_height: f64 = 0.0;
    for page in pages {
        let (width, height) = page.size();
        natural_width += width;
        natural_height = natural_height.max(height);
    }
    natural_width = natural_width.max(1.0);
    natural_height = natural_height.max(1.0);

    let available_width = (viewport_width - 2.0 * MARGIN - total_gap).max(1.0);
    let available_height = (viewport_height - 2.0 * MARGIN).max(1.0);
    let base_scale = match fit {
        FitMode::Page => (available_width / natural_width).min(available_height / natural_height),
        FitMode::Width => available_width / natural_width,
        FitMode::Height => available_height / natural_height,
        FitMode::Original => 96.0 / 72.0,
    };
    let scale = base_scale * zoom;
    let content_width = natural_width * scale + total_gap;
    let content_height = natural_height * scale;
    let max_pan_x = ((content_width - viewport_width) / 2.0).max(0.0);
    let max_pan_y = ((content_height - viewport_height) / 2.0).max(0.0);
    let pan_x = pan.0.clamp(-max_pan_x, max_pan_x);
    let pan_y = pan.1.clamp(-max_pan_y, max_pan_y);
    let mut x = (viewport_width - content_width) / 2.0 + pan_x;
    let y = (viewport_height - content_height) / 2.0 + pan_y;

    let mut rects = Vec::with_capacity(pages.len());
    for page in pages {
        let (point_width, point_height) = page.size();
        let width = point_width * scale;
        let height = point_height * scale;
        rects.push((x, y, width, height));
        x += width + PAGE_GAP;
    }

    SpreadLayout {
        rects,
        content_width,
        content_height,
    }
}

fn continuous_metrics(
    pages: &[PageImage],
    viewport_width: f64,
    zoom: f64,
) -> (f64, Vec<(f64, f64)>) {
    let display_width = (viewport_width - 2.0 * MARGIN).max(1.0) * zoom;
    let mut y = MARGIN;
    let mut positions = Vec::with_capacity(pages.len());
    for page in pages {
        let (_, point_height) = page.size();
        let height = display_width * point_height / page.point_width.max(1.0);
        positions.push((y, height));
        y += height + PAGE_GAP;
    }
    (y + MARGIN, positions)
}

fn continuous_max_scroll(
    pages: &[PageImage],
    viewport_width: f64,
    viewport_height: f64,
    zoom: f64,
) -> f64 {
    let (total_height, _) = continuous_metrics(pages, viewport_width, zoom);
    (total_height - viewport_height).max(0.0)
}

fn clamp_pan(state: &mut State, viewport_width: f64, viewport_height: f64) {
    if state.mode == CanvasMode::Continuous {
        let display_width = (viewport_width - 2.0 * MARGIN).max(1.0) * state.zoom;
        let max_pan_x = ((display_width - viewport_width) / 2.0).max(0.0);
        state.pan.0 = state.pan.0.clamp(-max_pan_x, max_pan_x);
        state.pan.1 = 0.0;
        return;
    }
    let layout = spread_layout(
        &state.current,
        viewport_width,
        viewport_height,
        state.fit,
        state.zoom,
        (0.0, 0.0),
    );
    let max_pan_x = ((layout.content_width - viewport_width) / 2.0).max(0.0);
    let max_pan_y = ((layout.content_height - viewport_height) / 2.0).max(0.0);
    state.pan.0 = state.pan.0.clamp(-max_pan_x, max_pan_x);
    state.pan.1 = state.pan.1.clamp(-max_pan_y, max_pan_y);
}

fn draw_continuous(
    snapshot: &gtk::Snapshot,
    pages: &[PageImage],
    viewport_width: f64,
    viewport_height: f64,
    state: &State,
) {
    if pages.is_empty() {
        return;
    }
    let display_width = (viewport_width - 2.0 * MARGIN).max(1.0) * state.zoom;
    let x = (viewport_width - display_width) / 2.0 + state.pan.0;
    let (_, positions) = continuous_metrics(pages, viewport_width, state.zoom);
    for (page, (top, height)) in pages.iter().zip(positions.iter()) {
        if *top - state.scroll + *height < 0.0 || *top - state.scroll > viewport_height {
            continue;
        }
        if let Some(texture) = &page.texture {
            snapshot.append_texture(
                texture,
                &graphene::Rect::new(
                    x as f32,
                    (*top - state.scroll) as f32,
                    display_width as f32,
                    *height as f32,
                ),
            );
        }
        draw_marks(
            snapshot,
            &state.marks,
            page.index,
            x,
            *top - state.scroll,
            display_width,
            *height,
        );
    }
}
