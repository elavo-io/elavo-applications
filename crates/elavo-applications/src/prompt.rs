//! The service's question window: a question or a notice, one at a time,
//! queued. It is made once and hidden between uses. GTK 4.22.5 destroys a
//! window's icon buffers twice when the window is finalized, which takes the
//! process down, so this window is never destroyed.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Plain,
    Suggested,
    Destructive,
}

/// Buttons in a row at the end, or stacked full width under the message.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Row,
    Stacked,
}

pub struct Choice {
    label: String,
    /// The label while Alt is held, for a button that does something else
    /// then.
    alt_label: Option<String>,
    style: Style,
}

impl Choice {
    pub fn new(label: &str, style: Style) -> Self {
        Choice {
            label: label.to_owned(),
            alt_label: None,
            style,
        }
    }

    pub fn alt(mut self, label: &str) -> Self {
        self.alt_label = Some(label.to_owned());
        self
    }
}

/// The button pressed, whether Alt was held, and whether the check box was
/// ticked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Answer {
    pub index: usize,
    pub alt: bool,
    pub checked: bool,
}

struct Ask {
    title: String,
    body: String,
    layout: Layout,
    choices: Vec<Choice>,
    check: Option<String>,
    reply: async_channel::Sender<Option<Answer>>,
}

pub struct Prompt {
    window: gtk::Window,
    title: gtk::Label,
    body: gtk::Label,
    check: gtk::CheckButton,
    buttons: gtk::Box,
    /// The shown buttons with their two labels.
    shown: RefCell<Vec<(gtk::Button, String, Option<String>)>>,
    /// The previous question's buttons. The clicked one is still in its
    /// clicked handler when the next question replaces it, so it is kept
    /// alive until the question after.
    retired: RefCell<Vec<gtk::Widget>>,
    alt: Cell<bool>,
    queue: RefCell<VecDeque<Ask>>,
    current: RefCell<Option<async_channel::Sender<Option<Answer>>>>,
    showing_notice: Cell<bool>,
}

impl Prompt {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::Window::builder()
            .application(app)
            .title("Applications")
            .default_width(440)
            .resizable(false)
            .hide_on_close(true)
            .build();
        // A title bar of its own: labwc drops a decoration request that
        // arrives before the surface has a view, and the window opens with
        // no frame.
        window.set_titlebar(Some(&gtk::HeaderBar::new()));
        let title = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["title-3"])
            .build();
        let body = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(52)
            .build();
        let check = gtk::CheckButton::builder().margin_top(4).build();
        let buttons = gtk::Box::builder().spacing(8).margin_top(10).build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(22)
            .margin_end(22)
            .build();
        content.append(&title);
        content.append(&body);
        content.append(&check);
        content.append(&buttons);
        window.set_child(Some(&content));
        let prompt = Rc::new(Prompt {
            window,
            title,
            body,
            check,
            buttons,
            shown: RefCell::default(),
            retired: RefCell::default(),
            alt: Cell::new(false),
            queue: RefCell::default(),
            current: RefCell::default(),
            showing_notice: Cell::new(false),
        });
        let weak = Rc::downgrade(&prompt);
        prompt.window.connect_close_request(move |_| {
            if let Some(p) = weak.upgrade() {
                p.answer(None);
            }
            glib::Propagation::Proceed
        });

        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&prompt);
        keys.connect_modifiers(move |_, state| {
            if let Some(p) = weak.upgrade() {
                p.set_alt(state.contains(gdk::ModifierType::ALT_MASK));
            }
            glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(&prompt);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(p) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            match key {
                gdk::Key::Escape => {
                    p.answer(None);
                    glib::Propagation::Stop
                }
                gdk::Key::Alt_L | gdk::Key::Alt_R => {
                    p.set_alt(true);
                    glib::Propagation::Proceed
                }
                _ => glib::Propagation::Proceed,
            }
        });
        let weak = Rc::downgrade(&prompt);
        keys.connect_key_released(move |_, key, _, _| {
            if let (Some(p), gdk::Key::Alt_L | gdk::Key::Alt_R) = (weak.upgrade(), key) {
                p.set_alt(false);
            }
        });
        prompt.window.add_controller(keys);
        let weak = Rc::downgrade(&prompt);
        prompt.window.connect_is_active_notify(move |w| {
            if let (Some(p), false) = (weak.upgrade(), w.is_active()) {
                p.set_alt(false);
            }
        });
        prompt
    }

    /// Shows a question and waits for a button. None when the window was
    /// closed another way.
    pub async fn ask(
        self: &Rc<Self>,
        title: &str,
        body: &str,
        buttons: &[(&str, Style)],
    ) -> Option<usize> {
        let choices = buttons.iter().map(|(l, s)| Choice::new(l, *s)).collect();
        self.choose(title, body, Layout::Row, choices)
            .await
            .map(|a| a.index)
    }

    /// [`ask`](Self::ask) with a layout and labels that change with Alt.
    pub async fn choose(
        self: &Rc<Self>,
        title: &str,
        body: &str,
        layout: Layout,
        choices: Vec<Choice>,
    ) -> Option<Answer> {
        self.enqueue(title, body, layout, choices, None).await
    }

    /// [`ask`](Self::ask) with an unticked check box above the buttons.
    pub async fn ask_with_check(
        self: &Rc<Self>,
        title: &str,
        body: &str,
        buttons: &[(&str, Style)],
        check: &str,
    ) -> Option<Answer> {
        let choices = buttons.iter().map(|(l, s)| Choice::new(l, *s)).collect();
        self.enqueue(title, body, Layout::Row, choices, Some(check.to_owned()))
            .await
    }

    async fn enqueue(
        self: &Rc<Self>,
        title: &str,
        body: &str,
        layout: Layout,
        choices: Vec<Choice>,
        check: Option<String>,
    ) -> Option<Answer> {
        let (tx, rx) = async_channel::bounded(1);
        self.queue.borrow_mut().push_back(Ask {
            title: title.to_owned(),
            body: body.to_owned(),
            layout,
            choices,
            check,
            reply: tx,
        });
        let idle = self.current.borrow().is_none();
        if idle {
            self.next();
        } else if self.showing_notice.get() {
            // A notice nobody dismissed must not hold back what comes next.
            self.answer(None);
        } else {
            self.window.present();
        }
        rx.recv().await.ok().flatten()
    }

    /// A message with an OK button. Does not wait.
    pub fn tell(self: &Rc<Self>, title: &str, body: &str) {
        let this = self.clone();
        let (title, body) = (title.to_owned(), body.to_owned());
        glib::spawn_future_local(async move {
            this.ask(&title, &body, &[("OK", Style::Suggested)]).await;
        });
    }

    fn set_alt(&self, alt: bool) {
        if self.alt.replace(alt) == alt {
            return;
        }
        for (button, label, alt_label) in self.shown.borrow().iter() {
            if let Some(alt_label) = alt_label {
                button.set_label(if alt { alt_label } else { label });
            }
        }
    }

    fn next(self: &Rc<Self>) {
        let Some(ask) = self.queue.borrow_mut().pop_front() else {
            self.window.set_visible(false);
            return;
        };
        self.title.set_label(&ask.title);
        self.body.set_label(&ask.body);
        self.body.set_visible(!ask.body.is_empty());
        self.check.set_active(false);
        self.check.set_label(ask.check.as_deref());
        self.check.set_visible(ask.check.is_some());
        let mut retired = Vec::new();
        while let Some(child) = self.buttons.first_child() {
            self.buttons.remove(&child);
            retired.push(child);
        }
        self.retired.replace(retired);
        let stacked = ask.layout == Layout::Stacked;
        self.buttons.set_orientation(if stacked {
            gtk::Orientation::Vertical
        } else {
            gtk::Orientation::Horizontal
        });
        self.buttons.set_halign(if stacked {
            gtk::Align::Fill
        } else {
            gtk::Align::End
        });
        let held = gdk::Display::default()
            .and_then(|d| d.default_seat())
            .and_then(|s| s.keyboard())
            .is_some_and(|k| k.modifier_state().contains(gdk::ModifierType::ALT_MASK));
        self.alt.set(held);
        self.window.set_default_widget(None::<&gtk::Widget>);
        let mut shown = Vec::new();
        for (i, choice) in ask.choices.iter().enumerate() {
            let label = match (&choice.alt_label, held) {
                (Some(alt), true) => alt,
                _ => &choice.label,
            };
            let button = gtk::Button::with_label(label);
            match choice.style {
                Style::Suggested => button.add_css_class("suggested-action"),
                Style::Destructive => button.add_css_class("destructive-action"),
                Style::Plain => {}
            }
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(p) = weak.upgrade() {
                    let alt = p.alt.get();
                    let checked = p.check.is_visible() && p.check.is_active();
                    p.answer(Some(Answer {
                        index: i,
                        alt,
                        checked,
                    }));
                }
            });
            self.buttons.append(&button);
            if choice.style == Style::Suggested {
                self.window.set_default_widget(Some(&button));
            }
            shown.push((button, choice.label.clone(), choice.alt_label.clone()));
        }
        self.shown.replace(shown);
        self.showing_notice
            .set(ask.choices.len() == 1 && ask.choices[0].style == Style::Suggested);
        self.current.replace(Some(ask.reply));
        self.window.present();
    }

    fn answer(self: &Rc<Self>, choice: Option<Answer>) {
        let Some(reply) = self.current.take() else {
            return;
        };
        let _ = reply.try_send(choice);
        if self.queue.borrow().is_empty() {
            self.window.set_visible(false);
        } else {
            self.next();
        }
    }
}
