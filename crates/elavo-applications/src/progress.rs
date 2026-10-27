//! A window for an install that takes a while: a progress bar in the accent
//! color and Cancel, then what went wrong if it failed. Windows are made
//! when needed and kept hidden for the next install, never destroyed, for
//! the same GTK 4.22.5 crash the question window avoids.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

const STYLE: &str = "progressbar.elavo-install > trough > progress { background-color: @accent_color; border-color: @accent_color; }";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Cancel,
    Retry,
    Close,
}

pub struct Progress {
    window: gtk::Window,
    title: gtk::Label,
    body: gtk::Label,
    bar: gtk::ProgressBar,
    buttons: gtk::Box,
    in_use: Cell<bool>,
    actions: RefCell<Option<async_channel::Sender<Action>>>,
}

impl Progress {
    fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::Window::builder()
            .application(app)
            .title("Applications")
            .default_width(440)
            .resizable(false)
            .hide_on_close(true)
            .build();
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
        let bar = gtk::ProgressBar::builder()
            .css_classes(["elavo-install"])
            .margin_top(6)
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
        content.append(&bar);
        content.append(&buttons);
        window.set_child(Some(&content));
        let progress = Rc::new(Progress {
            window,
            title,
            body,
            bar,
            buttons,
            in_use: Cell::new(false),
            actions: RefCell::default(),
        });
        let weak = Rc::downgrade(&progress);
        progress.window.connect_close_request(move |_| {
            if let Some(p) = weak.upgrade() {
                let closing = if p.bar.is_visible() {
                    Action::Cancel
                } else {
                    Action::Close
                };
                p.send(closing);
            }
            glib::Propagation::Proceed
        });
        progress
    }

    /// A free window from `pool`, made when every window is in use.
    pub fn take(pool: &RefCell<Vec<Rc<Progress>>>, app: &gtk::Application) -> Rc<Progress> {
        if pool.borrow().is_empty() {
            if let Some(display) = gdk::Display::default() {
                let css = gtk::CssProvider::new();
                css.load_from_string(STYLE);
                gtk::style_context_add_provider_for_display(
                    &display,
                    &css,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
            }
        }
        let free = pool.borrow().iter().find(|p| !p.in_use.get()).cloned();
        let progress = free.unwrap_or_else(|| {
            let made = Progress::new(app);
            pool.borrow_mut().push(made.clone());
            made
        });
        progress.in_use.set(true);
        progress
    }

    /// Shows the window with an empty bar and Cancel. What the buttons are
    /// pressed for arrives on the returned channel.
    pub fn start(self: &Rc<Self>, title: &str, body: &str) -> async_channel::Receiver<Action> {
        let (tx, rx) = async_channel::unbounded();
        self.actions.replace(Some(tx));
        self.title.set_label(title);
        self.body.set_label(body);
        self.bar.set_fraction(0.0);
        self.bar.set_visible(true);
        self.set_buttons(&[("Cancel", Action::Cancel, false)]);
        self.window.present();
        rx
    }

    pub fn set_fraction(&self, fraction: f64) {
        self.bar.set_fraction(fraction);
    }

    /// Replaces the bar with what went wrong, and offers Try Again. The
    /// answer arrives on the returned channel.
    pub fn failed(self: &Rc<Self>, title: &str, body: &str) -> async_channel::Receiver<Action> {
        let (tx, rx) = async_channel::unbounded();
        self.actions.replace(Some(tx));
        self.title.set_label(title);
        self.body.set_label(body);
        self.bar.set_visible(false);
        self.set_buttons(&[
            ("Try Again", Action::Retry, false),
            ("Close", Action::Close, true),
        ]);
        self.window.present();
        rx
    }

    /// Hides the window and gives it back to the pool.
    pub fn finish(&self) {
        self.actions.replace(None);
        self.window.set_visible(false);
        self.in_use.set(false);
    }

    fn send(&self, action: Action) {
        if let Some(tx) = self.actions.borrow().as_ref() {
            let _ = tx.try_send(action);
        }
    }

    fn set_buttons(self: &Rc<Self>, buttons: &[(&str, Action, bool)]) {
        while let Some(child) = self.buttons.first_child() {
            self.buttons.remove(&child);
        }
        self.window.set_default_widget(None::<&gtk::Widget>);
        for &(label, action, suggested) in buttons {
            let button = gtk::Button::with_label(label);
            if suggested {
                button.add_css_class("suggested-action");
                self.window.set_default_widget(Some(&button));
            }
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(p) = weak.upgrade() {
                    p.send(action);
                }
            });
            self.buttons.append(&button);
        }
    }
}
