//! The one window the service shows: a question or a notice, one at a time,
//! queued. It is made once and hidden between uses. GTK 4.22.5 destroys a
//! window's icon buffers twice when the window is finalized, which takes the
//! process down, so this window is never destroyed.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use gtk::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Plain,
    Suggested,
    Destructive,
}

struct Ask {
    title: String,
    body: String,
    buttons: Vec<(String, Style)>,
    reply: async_channel::Sender<Option<usize>>,
}

pub struct Prompt {
    window: gtk::Window,
    title: gtk::Label,
    body: gtk::Label,
    buttons: gtk::Box,
    queue: RefCell<VecDeque<Ask>>,
    current: RefCell<Option<async_channel::Sender<Option<usize>>>>,
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
        let buttons = gtk::Box::builder()
            .spacing(8)
            .halign(gtk::Align::End)
            .margin_top(10)
            .build();
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
        content.append(&buttons);
        window.set_child(Some(&content));
        let prompt = Rc::new(Prompt {
            window,
            title,
            body,
            buttons,
            queue: RefCell::default(),
            current: RefCell::default(),
            showing_notice: Cell::new(false),
        });
        let weak = Rc::downgrade(&prompt);
        prompt.window.connect_close_request(move |_| {
            if let Some(p) = weak.upgrade() {
                p.answer(None);
            }
            gtk::glib::Propagation::Proceed
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
        let (tx, rx) = async_channel::bounded(1);
        self.queue.borrow_mut().push_back(Ask {
            title: title.to_owned(),
            body: body.to_owned(),
            buttons: buttons.iter().map(|(l, s)| ((*l).to_owned(), *s)).collect(),
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
        gtk::glib::spawn_future_local(async move {
            this.ask(&title, &body, &[("OK", Style::Suggested)]).await;
        });
    }

    fn next(self: &Rc<Self>) {
        let Some(ask) = self.queue.borrow_mut().pop_front() else {
            self.window.set_visible(false);
            return;
        };
        self.title.set_label(&ask.title);
        self.body.set_label(&ask.body);
        self.body.set_visible(!ask.body.is_empty());
        while let Some(child) = self.buttons.first_child() {
            self.buttons.remove(&child);
        }
        self.window.set_default_widget(None::<&gtk::Widget>);
        for (i, (label, style)) in ask.buttons.iter().enumerate() {
            let button = gtk::Button::with_label(label);
            match style {
                Style::Suggested => button.add_css_class("suggested-action"),
                Style::Destructive => button.add_css_class("destructive-action"),
                Style::Plain => {}
            }
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(p) = weak.upgrade() {
                    p.answer(Some(i));
                }
            });
            self.buttons.append(&button);
            if *style == Style::Suggested {
                self.window.set_default_widget(Some(&button));
            }
        }
        self.showing_notice
            .set(ask.buttons.len() == 1 && ask.buttons[0].1 == Style::Suggested);
        self.current.replace(Some(ask.reply));
        self.window.present();
    }

    fn answer(self: &Rc<Self>, choice: Option<usize>) {
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
