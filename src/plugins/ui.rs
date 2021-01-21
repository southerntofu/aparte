/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */
use chrono::offset::{Local, TimeZone};
use chrono::Local as LocalTz;
use futures::task::{AtomicWaker, Context, Poll};
use futures::Stream;
use linked_hash_set::LinkedHashSet;
use std::cell::RefCell;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io::Error as IoError;
use std::io::{Read, Stdout, Write};
use std::pin::Pin;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use termion::color;
use termion::event::{parse_event as termion_parse_event, Event as TermionEvent, Key};
use termion::get_tty;
use termion::raw::{IntoRawMode, RawTerminal};
use termion::screen::AlternateScreen;
use uuid::Uuid;
use xmpp_parsers::{BareJid, Jid};

use crate::account::Account;
use crate::color::id_to_rgb;
use crate::command::Command;
use crate::core::{Aparte, Event, Plugin};
use crate::cursor::Cursor;
use crate::message::{Message, XmppMessage};
use crate::terminus::{
    BufferedWin, Dimension, FrameLayout, Input, LinearLayout, ListView, Orientation, View,
    ViewTrait, Window as _,
};
use crate::{contact, conversation};

type Screen = AlternateScreen<RawTerminal<Stdout>>;

#[derive(Debug, Clone)]
enum ConversationKind {
    Chat,
    Group,
}

#[derive(Debug, Clone)]
struct Conversation {
    account: Account,
    jid: BareJid,
    kind: ConversationKind,
}

enum UIEvent {
    Core(Event),
    Validate(Rc<RefCell<Option<(String, bool)>>>),
    GetInput(Rc<RefCell<Option<(String, Cursor, bool)>>>),
    AddWindow(String, Option<Box<dyn ViewTrait<UIEvent>>>),
}

struct TitleBar {
    window_name: Option<String>,
}

impl View<'_, TitleBar, UIEvent> {
    fn new(screen: Rc<RefCell<Screen>>) -> Self {
        Self {
            screen: screen,
            width: Dimension::MatchParent,
            height: Dimension::Absolute(1),
            x: 0,
            y: 0,
            w: None,
            h: None,
            dirty: true,
            visible: true,
            #[cfg(feature = "no-cursor-save")]
            cursor_x: None,
            #[cfg(feature = "no-cursor-save")]
            cursor_y: None,
            content: TitleBar { window_name: None },
            event_handler: None,
        }
    }

    fn set_name(&mut self, name: &str) {
        self.content.window_name = Some(name.to_string());
        self.redraw();
    }
}

impl ViewTrait<UIEvent> for View<'_, TitleBar, UIEvent> {
    fn redraw(&mut self) {
        self.save_cursor();

        {
            let mut screen = self.screen.borrow_mut();

            write!(screen, "{}", termion::cursor::Goto(self.x, self.y)).unwrap();
            write!(
                screen,
                "{}{}",
                color::Bg(color::Blue),
                color::Fg(color::White)
            )
            .unwrap();

            for _ in 0..self.w.unwrap() {
                write!(screen, " ").unwrap();
            }
            write!(screen, "{}", termion::cursor::Goto(self.x, self.y)).unwrap();
            if let Some(window_name) = &self.content.window_name {
                write!(screen, " {}", window_name).unwrap();
            }

            write!(
                screen,
                "{}{}",
                color::Bg(color::Reset),
                color::Fg(color::Reset)
            )
            .unwrap();
        }

        self.restore_cursor();
        while let Err(_) = self.screen.borrow_mut().flush() {}
    }

    fn event(&mut self, event: &mut UIEvent) {
        match event {
            UIEvent::Core(Event::ChangeWindow(name)) => {
                self.set_name(name);
            }
            _ => {}
        }
    }
}

struct WinBar {
    connection: Option<String>,
    windows: Vec<String>,
    current_window: Option<String>,
    highlighted: Vec<String>,
}

impl View<'_, WinBar, UIEvent> {
    fn new(screen: Rc<RefCell<Screen>>) -> Self {
        Self {
            screen: screen,
            width: Dimension::MatchParent,
            height: Dimension::Absolute(1),
            x: 0,
            y: 0,
            w: None,
            h: None,
            dirty: true,
            visible: true,
            #[cfg(feature = "no-cursor-save")]
            cursor_x: None,
            #[cfg(feature = "no-cursor-save")]
            cursor_y: None,
            content: WinBar {
                connection: None,
                windows: Vec::new(),
                current_window: None,
                highlighted: Vec::new(),
            },
            event_handler: None,
        }
    }

    fn add_window(&mut self, window: &str) {
        self.content.windows.push(window.to_string());
        self.redraw();
    }

    fn set_current_window(&mut self, window: &str) {
        self.content.current_window = Some(window.to_string());
        self.content.highlighted.drain_filter(|w| w == &window);
        self.redraw();
    }

    fn highlight_window(&mut self, window: &str) {
        if self
            .content
            .highlighted
            .iter()
            .find(|w| w == &window)
            .is_none()
        {
            self.content.highlighted.push(window.to_string());
            self.redraw();
        }
    }
}

impl ViewTrait<UIEvent> for View<'_, WinBar, UIEvent> {
    fn redraw(&mut self) {
        self.save_cursor();

        {
            let mut screen = self.screen.borrow_mut();
            let mut written = 0;

            write!(screen, "{}", termion::cursor::Goto(self.x, self.y)).unwrap();
            write!(
                screen,
                "{}{}",
                color::Bg(color::Blue),
                color::Fg(color::White)
            )
            .unwrap();

            for _ in 0..self.w.unwrap() {
                write!(screen, " ").unwrap();
            }

            write!(screen, "{}", termion::cursor::Goto(self.x, self.y)).unwrap();
            if let Some(connection) = &self.content.connection {
                write!(screen, " {}", connection).unwrap();
                written += 1 + connection.len();
            }

            if let Some(current_window) = &self.content.current_window {
                write!(
                    screen,
                    " [{}{}{}]",
                    termion::style::Bold,
                    current_window,
                    termion::style::NoBold
                )
                .unwrap();
                written += 3 + current_window.len();
            } else {
                write!(screen, " []").unwrap();
                written += 3;
            }

            let mut first = true;
            for window in &self.content.highlighted {
                // Keep space for at least ", …]"
                if window.len() + written + 4 > self.w.unwrap() as usize {
                    if !first {
                        write!(screen, ", …").unwrap();
                    }
                    break;
                }

                if first {
                    write!(screen, " [").unwrap();
                    written += 3; // Also count the closing bracket
                    first = false;
                } else {
                    write!(screen, ", ").unwrap();
                    written += 2;
                }
                write!(
                    screen,
                    "{}{}{}",
                    termion::style::Bold,
                    window,
                    termion::style::NoBold
                )
                .unwrap();
                written += window.len();
            }

            if !first {
                write!(screen, "]").unwrap();
            }

            write!(
                screen,
                "{}{}",
                color::Bg(color::Reset),
                color::Fg(color::Reset)
            )
            .unwrap();
        }

        self.restore_cursor();
        while let Err(_) = self.screen.borrow_mut().flush() {}
    }

    fn event(&mut self, event: &mut UIEvent) {
        match event {
            UIEvent::Core(Event::ChangeWindow(name)) => {
                self.set_current_window(name);
            }
            UIEvent::AddWindow(name, _) => {
                self.add_window(name);
            }
            UIEvent::Core(Event::Connected(account, _)) => {
                self.content.connection = Some(account.to_string());
                self.redraw();
            }
            UIEvent::Core(Event::Message(_, Message::Incoming(XmppMessage::Chat(message)))) => {
                let mut highlighted = None;
                for window in &self.content.windows {
                    if &message.from.to_string() == window
                        && Some(window) != self.content.current_window.as_ref()
                    {
                        highlighted = Some(window.clone());
                    }
                }
                if highlighted.is_some() {
                    self.highlight_window(&highlighted.unwrap());
                    self.redraw();
                }
            }
            UIEvent::Core(Event::Message(
                _,
                Message::Incoming(XmppMessage::Groupchat(message)),
            )) => {
                let mut highlighted = None;
                for window in &self.content.windows {
                    if &message.from.to_string() == window
                        && Some(window) != self.content.current_window.as_ref()
                    {
                        highlighted = Some(window.clone());
                    }
                }
                if highlighted.is_some() {
                    self.highlight_window(&highlighted.unwrap());
                    self.redraw();
                }
            }
            _ => {}
        }
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Message::Log(message) => {
                let timestamp = Local.from_utc_datetime(&message.timestamp.naive_local());
                for line in message.body.lines() {
                    write!(
                        f,
                        "{}{} - {}\n",
                        color::Fg(color::White),
                        timestamp.format("%T"),
                        line
                    )?;
                }

                Ok(())
            }
            Message::Incoming(XmppMessage::Chat(message)) => {
                let timestamp = Local.from_utc_datetime(&message.timestamp.naive_local());
                let padding_len = format!("{} - {}: ", timestamp.format("%T"), message.from).len();
                let padding = " ".repeat(padding_len);

                let (r, g, b) = id_to_rgb(&message.from.to_string());
                write!(
                    f,
                    "{}{} - {}{}:{} ",
                    color::Fg(color::White),
                    timestamp.format("%T"),
                    color::Fg(color::Rgb(r, g, b)),
                    message.from,
                    color::Fg(color::White)
                )?;

                let mut iter = message.body.lines();
                if let Some(line) = iter.next() {
                    write!(f, "{}", line)?;
                }
                while let Some(line) = iter.next() {
                    write!(f, "\n{}{}", padding, line)?;
                }

                Ok(())
            }
            Message::Outgoing(XmppMessage::Chat(message)) => {
                let timestamp = Local.from_utc_datetime(&message.timestamp.naive_local());
                write!(
                    f,
                    "{}{} - {}me:{} {}",
                    color::Fg(color::White),
                    timestamp.format("%T"),
                    color::Fg(color::Yellow),
                    color::Fg(color::White),
                    message.body
                )
            }
            Message::Incoming(XmppMessage::Groupchat(message)) => {
                if let Jid::Full(from) = &message.from_full {
                    let timestamp = Local.from_utc_datetime(&message.timestamp.naive_local());
                    let padding_len =
                        format!("{} - {}: ", timestamp.format("%T"), from.resource).len();
                    let padding = " ".repeat(padding_len);

                    let (r, g, b) = id_to_rgb(&from.resource);
                    write!(
                        f,
                        "{}{} - {}{}:{} ",
                        color::Fg(color::White),
                        timestamp.format("%T"),
                        color::Fg(color::Rgb(r, g, b)),
                        from.resource,
                        color::Fg(color::White)
                    )?;

                    let mut iter = message.body.lines();
                    if let Some(line) = iter.next() {
                        write!(f, "{}", line)?;
                    }
                    while let Some(line) = iter.next() {
                        write!(f, "\n{}{}", padding, line)?;
                    }
                }
                Ok(())
            }
            Message::Outgoing(XmppMessage::Groupchat(message)) => {
                let timestamp = Local.from_utc_datetime(&message.timestamp.naive_local());
                write!(
                    f,
                    "{}{} - {}me:{} {}",
                    color::Fg(color::White),
                    timestamp.format("%T"),
                    color::Fg(color::Yellow),
                    color::Fg(color::White),
                    message.body
                )
            }
        }
    }
}

impl fmt::Display for contact::Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}{}",
            color::Fg(color::Yellow),
            self.0,
            color::Fg(color::White)
        )
    }
}

impl fmt::Display for contact::Contact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.presence {
            contact::Presence::Available | contact::Presence::Chat => {
                write!(f, "{}", color::Fg(color::Green))?
            }
            contact::Presence::Away
            | contact::Presence::Dnd
            | contact::Presence::Xa
            | contact::Presence::Unavailable => write!(f, "{}", color::Fg(color::White))?,
        };

        match &self.name {
            Some(name) => write!(f, "{} ({}){}", name, self.jid, color::Fg(color::White)),
            None => write!(f, "{}{}", self.jid, color::Fg(color::White)),
        }
    }
}

impl fmt::Display for contact::Bookmark {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(name) => write!(f, "{}{}", name, color::Fg(color::White)),
            None => write!(f, "{}{}", self.jid, color::Fg(color::White)),
        }
    }
}

#[derive(Clone, Debug, Ord, PartialOrd)]
pub enum RosterItem {
    Contact(contact::Contact),
    Bookmark(contact::Bookmark),
    Window(String),
}

impl Hash for RosterItem {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Contact(contact) => contact.jid.hash(state),
            Self::Bookmark(bookmark) => bookmark.jid.hash(state),
            Self::Window(window) => window.hash(state),
        };
    }
}

impl PartialEq for RosterItem {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Contact(a), Self::Contact(b)) => a.eq(b),
            (Self::Bookmark(a), Self::Bookmark(b)) => a.eq(b),
            (Self::Window(a), Self::Window(b)) => a.eq(b),
            _ => false,
        }
    }
}

impl Eq for RosterItem {}

impl fmt::Display for RosterItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self {
            Self::Contact(contact) => contact.fmt(f),
            Self::Bookmark(bookmark) => bookmark.fmt(f),
            Self::Window(window) => write!(f, "{}", window),
        }
    }
}

impl fmt::Display for conversation::Occupant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (r, g, b) = id_to_rgb(&self.nick);
        write!(
            f,
            "{}{}{}",
            color::Fg(color::Rgb(r, g, b)),
            self.nick,
            color::Fg(color::White)
        )
    }
}

impl fmt::Display for conversation::Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            conversation::Role::Moderator => write!(
                f,
                "{}Moderators{}",
                color::Fg(color::Yellow),
                color::Fg(color::Yellow)
            ),
            conversation::Role::Participant => write!(
                f,
                "{}Participants{}",
                color::Fg(color::Yellow),
                color::Fg(color::Yellow)
            ),
            conversation::Role::Visitor => write!(
                f,
                "{}Visitors{}",
                color::Fg(color::Yellow),
                color::Fg(color::Yellow)
            ),
            conversation::Role::None => write!(
                f,
                "{}Others{}",
                color::Fg(color::Yellow),
                color::Fg(color::Yellow)
            ),
        }
    }
}

pub struct UIPlugin {
    screen: Rc<RefCell<Screen>>,
    windows: Vec<String>,
    current_window: Option<String>,
    unread_windows: LinkedHashSet<String>,
    conversations: HashMap<String, Conversation>,
    root: Box<dyn ViewTrait<UIEvent>>,
    password_command: Option<Command>,
}

impl UIPlugin {
    pub fn event_stream(&self) -> EventStream {
        EventStream::new()
    }

    fn add_conversation(&mut self, _aparte: &mut Aparte, conversation: Conversation) {
        let jid = conversation.jid.clone();
        match conversation.kind {
            ConversationKind::Chat => {
                let chat = View::<BufferedWin<Message>, UIEvent>::new(self.screen.clone())
                    .with_event(move |view, event| {
                        match event {
                            UIEvent::Core(Event::Message(
                                _,
                                Message::Incoming(XmppMessage::Chat(message)),
                            )) => {
                                // TODO check to == us
                                if message.from == jid {
                                    view.recv_message(&Message::Incoming(XmppMessage::Chat(
                                        message.clone(),
                                    )));
                                    view.bell();
                                }
                            }
                            UIEvent::Core(Event::Message(
                                _,
                                Message::Outgoing(XmppMessage::Chat(message)),
                            )) => {
                                // TODO check from == us
                                if message.to == jid {
                                    view.recv_message(&Message::Outgoing(XmppMessage::Chat(
                                        message.clone(),
                                    )));
                                }
                            }
                            UIEvent::Core(Event::Key(Key::PageUp)) => {
                                //aparte.schedule(Event::LoadHistory(jid.clone()));
                                view.page_up();
                            }
                            UIEvent::Core(Event::Key(Key::PageDown)) => view.page_down(),
                            _ => {}
                        }
                    });

                self.windows.push(conversation.jid.to_string());
                self.root.event(&mut UIEvent::AddWindow(
                    conversation.jid.to_string(),
                    Some(Box::new(chat)),
                ));
                self.conversations
                    .insert(conversation.jid.to_string(), conversation);
            }
            ConversationKind::Group => {
                let mut layout = View::<LinearLayout<UIEvent>, UIEvent>::new(
                    self.screen.clone(),
                    Orientation::Horizontal,
                    Dimension::MatchParent,
                    Dimension::MatchParent,
                )
                .with_event(|layout, event| {
                    for child in layout.content.children.iter_mut() {
                        child.event(event);
                    }
                });

                let chat_jid = jid.clone();
                let chat = View::<BufferedWin<Message>, UIEvent>::new(self.screen.clone())
                    .with_event(move |view, event| {
                        match event {
                            UIEvent::Core(Event::Message(
                                _,
                                Message::Incoming(XmppMessage::Groupchat(message)),
                            )) => {
                                // TODO check to == us
                                if message.from == chat_jid {
                                    view.recv_message(&Message::Incoming(XmppMessage::Groupchat(
                                        message.clone(),
                                    )));
                                    view.bell();
                                }
                            }
                            UIEvent::Core(Event::Message(
                                _,
                                Message::Outgoing(XmppMessage::Groupchat(message)),
                            )) => {
                                // TODO check from == us
                                if message.to == chat_jid {
                                    view.recv_message(&Message::Outgoing(XmppMessage::Groupchat(
                                        message.clone(),
                                    )));
                                }
                            }
                            UIEvent::Core(Event::Key(Key::PageUp)) => view.page_up(),
                            UIEvent::Core(Event::Key(Key::PageDown)) => view.page_down(),
                            _ => {}
                        }
                    });
                layout.push(chat);

                let roster_jid = jid.clone();
                let roster =
                    View::<ListView<conversation::Role, conversation::Occupant>, UIEvent>::new(
                        self.screen.clone(),
                    )
                    .with_none_group()
                    .with_unique_item()
                    .with_sort_item()
                    .with_event(move |view, event| match event {
                        UIEvent::Core(Event::Occupant {
                            conversation,
                            occupant,
                            ..
                        }) => {
                            if roster_jid == *conversation {
                                view.insert(occupant.clone(), Some(occupant.role));
                            }
                        }
                        _ => {}
                    });
                layout.push(roster);

                self.windows.push(conversation.jid.to_string());
                self.root.event(&mut UIEvent::AddWindow(
                    conversation.jid.to_string(),
                    Some(Box::new(layout)),
                ));
                self.conversations
                    .insert(conversation.jid.to_string(), conversation);
            }
        }
    }

    pub fn change_window(&mut self, window: &str) {
        self.root
            .event(&mut UIEvent::Core(Event::ChangeWindow(window.to_string())));
        self.current_window = Some(window.to_string());
    }

    #[allow(unused)] // XXX Should be used when alt+arrow is fixed see https://gitlab.redox-os.org/redox-os/termion/-/issues/183
    pub fn next_window(&mut self) {
        if let Some(current) = &self.current_window {
            let index = self.windows.iter().position(|e| e == current).unwrap();
            if index < self.windows.len() - 1 {
                self.change_window(&self.windows[index + 1].clone());
            }
        } else if self.windows.len() > 0 {
            self.change_window(&self.windows[0].clone());
        }
    }

    #[allow(unused)] // XXX Should be used when alt+arrow is fixed see https://gitlab.redox-os.org/redox-os/termion/-/issues/183
    pub fn prev_window(&mut self) {
        if let Some(current) = &self.current_window {
            let index = self.windows.iter().position(|e| e == current).unwrap();
            if index > 0 {
                self.change_window(&self.windows[index - 1].clone());
            }
        } else if self.windows.len() > 0 {
            self.change_window(&self.windows[0].clone());
        }
    }

    pub fn get_windows(&self) -> Vec<String> {
        self.windows.clone()
    }
}

impl Plugin for UIPlugin {
    fn new() -> Self {
        let stdout = std::io::stdout().into_raw_mode().unwrap();
        let screen = Rc::new(RefCell::new(AlternateScreen::from(stdout)));
        let mut layout = View::<LinearLayout<UIEvent>, UIEvent>::new(
            screen.clone(),
            Orientation::Vertical,
            Dimension::MatchParent,
            Dimension::MatchParent,
        )
        .with_event(|layout, event| {
            for child in layout.content.children.iter_mut() {
                child.event(event);
            }

            if layout.is_dirty() {
                layout.measure(layout.w, layout.h);
                layout.layout(layout.x, layout.y);
                layout.redraw();
            }
        });

        let title_bar = View::<TitleBar, UIEvent>::new(screen.clone());
        let frame = View::<FrameLayout<String, UIEvent>, UIEvent>::new(screen.clone()).with_event(
            |frame, event| {
                match event {
                    UIEvent::Core(Event::ChangeWindow(name)) => {
                        frame.current(name.to_string());
                    }
                    UIEvent::AddWindow(name, view) => {
                        let view = view.take().unwrap();
                        frame.insert(name.to_string(), view);
                    }
                    _ => {}
                }
                for (_, child) in frame.content.children.iter_mut() {
                    child.event(event);
                }
            },
        );
        let win_bar = View::<WinBar, UIEvent>::new(screen.clone());
        let input =
            View::<Input, UIEvent>::new(screen.clone()).with_event(|input, event| match event {
                UIEvent::Core(Event::Key(Key::Char(c))) => input.key(*c),
                UIEvent::Core(Event::Key(Key::Backspace)) => input.backspace(),
                UIEvent::Core(Event::Key(Key::Delete)) => input.delete(),
                UIEvent::Core(Event::Key(Key::Home)) => input.home(),
                UIEvent::Core(Event::Key(Key::End)) => input.end(),
                UIEvent::Core(Event::Key(Key::Up)) => input.previous(),
                UIEvent::Core(Event::Key(Key::Down)) => input.next(),
                UIEvent::Core(Event::Key(Key::Left)) => input.left(),
                UIEvent::Core(Event::Key(Key::Right)) => input.right(),
                UIEvent::Core(Event::Key(Key::Ctrl('a'))) => input.home(),
                UIEvent::Core(Event::Key(Key::Ctrl('b'))) => input.left(),
                UIEvent::Core(Event::Key(Key::Ctrl('e'))) => input.end(),
                UIEvent::Core(Event::Key(Key::Ctrl('f'))) => input.right(),
                UIEvent::Core(Event::Key(Key::Ctrl('h'))) => input.backspace(),
                UIEvent::Core(Event::Key(Key::Ctrl('w'))) => input.backward_delete_word(),
                UIEvent::Core(Event::Key(Key::Ctrl('u'))) => input.delete_from_cursor_to_start(),
                UIEvent::Core(Event::Key(Key::Ctrl('k'))) => input.delete_from_cursor_to_end(),
                UIEvent::Validate(result) => {
                    let mut result = result.borrow_mut();
                    result.replace(input.validate());
                }
                UIEvent::GetInput(result) => {
                    let mut result = result.borrow_mut();
                    result.replace((
                        input.content.buf.clone(),
                        input.content.cursor.clone(),
                        input.content.password,
                    ));
                }
                UIEvent::Core(Event::Completed(raw_buf, cursor)) => {
                    input.content.buf = raw_buf.clone();
                    input.content.cursor = cursor.clone();
                    input.redraw();
                }
                UIEvent::Core(Event::ReadPassword(_)) => input.password(),
                _ => {}
            });

        layout.push(title_bar);
        layout.push(frame);
        layout.push(win_bar);
        layout.push(input);

        Self {
            screen: screen,
            root: Box::new(layout),
            windows: Vec::new(),
            unread_windows: LinkedHashSet::new(),
            current_window: None,
            conversations: HashMap::new(),
            password_command: None,
        }
    }

    fn init(&mut self, _aparte: &mut Aparte) -> Result<(), ()> {
        {
            let mut screen = self.screen.borrow_mut();
            write!(screen, "{}", termion::clear::All).unwrap();
        }

        let (width, height) = termion::terminal_size().unwrap();
        self.root.measure(Some(width), Some(height));
        self.root.layout(1, 1);
        self.root.redraw();

        let mut console = View::<LinearLayout<UIEvent>, UIEvent>::new(
            self.screen.clone(),
            Orientation::Horizontal,
            Dimension::MatchParent,
            Dimension::MatchParent,
        )
        .with_event(|layout, event| {
            for child in layout.content.children.iter_mut() {
                child.event(event);
            }
        });
        console.push(
            View::<BufferedWin<Message>, UIEvent>::new(self.screen.clone()).with_event(
                |view, event| match event {
                    UIEvent::Core(Event::Message(_, Message::Log(message))) => {
                        view.recv_message(&Message::Log(message.clone()));
                    }
                    UIEvent::Core(Event::Key(Key::PageUp)) => view.page_up(),
                    UIEvent::Core(Event::Key(Key::PageDown)) => view.page_down(),
                    _ => {}
                },
            ),
        );
        let roster =
            View::<ListView<contact::Group, RosterItem>, UIEvent>::new(self.screen.clone())
                .with_none_group()
                .with_sort_item()
                .with_event(|view, event| match event {
                    UIEvent::Core(Event::Connected(_, _)) => {
                        view.add_group(contact::Group(String::from("Windows")));
                        view.add_group(contact::Group(String::from("Contacts")));
                        view.add_group(contact::Group(String::from("Bookmarks")));
                    }
                    UIEvent::Core(Event::Contact(_, contact))
                    | UIEvent::Core(Event::ContactUpdate(_, contact)) => {
                        if contact.groups.len() > 0 {
                            for group in &contact.groups {
                                view.insert(
                                    RosterItem::Contact(contact.clone()),
                                    Some(group.clone()),
                                );
                            }
                        } else {
                            let group = contact::Group(String::from("Contacts"));
                            view.insert(RosterItem::Contact(contact.clone()), Some(group));
                        }
                    }
                    UIEvent::Core(Event::Bookmark(bookmark)) => {
                        let group = contact::Group(String::from("Bookmarks"));
                        view.insert(RosterItem::Bookmark(bookmark.clone()), Some(group));
                    }
                    UIEvent::Core(Event::DeletedBookmark(jid)) => {
                        let group = contact::Group(String::from("Bookmarks"));
                        let bookmark = contact::Bookmark {
                            jid: jid.clone(),
                            name: None,
                            nick: None,
                            password: None,
                            autojoin: false,
                            extensions: None,
                        };
                        let _ = view.remove(RosterItem::Bookmark(bookmark.clone()), Some(group));
                    }
                    UIEvent::AddWindow(name, _) => {
                        debug!("test");
                        let group = contact::Group(String::from("Windows"));
                        view.insert(RosterItem::Window(name.clone()), Some(group));
                    }
                    _ => {}
                });
        console.push(roster);

        self.windows.push("console".to_string());
        self.root.event(&mut UIEvent::AddWindow(
            "console".to_string(),
            Some(Box::new(console)),
        ));
        self.change_window("console");

        Ok(())
    }

    fn on_event(&mut self, aparte: &mut Aparte, event: &Event) {
        match event {
            Event::ReadPassword(command) => {
                self.password_command = Some(command.clone());
                self.root
                    .event(&mut UIEvent::Core(Event::ReadPassword(command.clone())));
            }
            Event::Connected(account, jid) => {
                self.root.event(&mut UIEvent::Core(Event::Connected(
                    account.clone(),
                    jid.clone(),
                )));
            }
            Event::Message(account, message) => {
                match message {
                    Message::Incoming(XmppMessage::Chat(message)) => {
                        let window_name = message.from.to_string();
                        if !self.conversations.contains_key(&window_name) {
                            self.add_conversation(
                                aparte,
                                Conversation {
                                    account: account.clone().unwrap(),
                                    jid: BareJid::from_str(&window_name).unwrap(),
                                    kind: ConversationKind::Chat,
                                },
                            );
                        }

                        let mut unread = None;
                        for window in &self.windows {
                            if &message.from.to_string() == window
                                && Some(window) != self.current_window.as_ref()
                            {
                                unread = Some(window.clone());
                            }
                        }
                        if unread.is_some() {
                            self.unread_windows.insert(unread.unwrap());
                        }
                    }
                    Message::Outgoing(XmppMessage::Chat(message)) => {
                        let window_name = message.to.to_string();
                        if !self.conversations.contains_key(&window_name) {
                            self.add_conversation(
                                aparte,
                                Conversation {
                                    account: account.clone().unwrap(),
                                    jid: BareJid::from_str(&window_name).unwrap(),
                                    kind: ConversationKind::Chat,
                                },
                            );
                        }
                    }
                    Message::Incoming(XmppMessage::Groupchat(message)) => {
                        let window_name = message.from.to_string();
                        if !self.conversations.contains_key(&window_name) {
                            self.add_conversation(
                                aparte,
                                Conversation {
                                    account: account.clone().unwrap(),
                                    jid: BareJid::from_str(&window_name).unwrap(),
                                    kind: ConversationKind::Group,
                                },
                            );
                        }

                        let mut unread = None;
                        for window in &self.windows {
                            if &message.from.to_string() == window
                                && Some(window) != self.current_window.as_ref()
                            {
                                unread = Some(window.clone());
                            }
                        }
                        if unread.is_some() {
                            self.unread_windows.insert(unread.unwrap());
                        }
                    }
                    Message::Outgoing(XmppMessage::Groupchat(message)) => {
                        let window_name = message.to.to_string();
                        if !self.conversations.contains_key(&window_name) {
                            self.add_conversation(
                                aparte,
                                Conversation {
                                    account: account.clone().unwrap(),
                                    jid: BareJid::from_str(&window_name).unwrap(),
                                    kind: ConversationKind::Group,
                                },
                            );
                        }
                    }
                    Message::Log(_message) => {}
                };

                self.root.event(&mut UIEvent::Core(Event::Message(
                    account.clone(),
                    message.clone(),
                )));
            }
            Event::Chat { account, contact } => {
                // Should we store account association?
                let win_name = contact.to_string();
                if !self.conversations.contains_key(&win_name) {
                    self.add_conversation(
                        aparte,
                        Conversation {
                            account: account.clone(),
                            jid: BareJid::from_str(&win_name).unwrap(),
                            kind: ConversationKind::Chat,
                        },
                    );
                }
                self.change_window(&win_name);
            }
            Event::Joined {
                account,
                channel,
                user_request,
            } => {
                let bare: BareJid = channel.clone().into();
                let win_name = bare.to_string();
                if !self.conversations.contains_key(&win_name) {
                    self.add_conversation(
                        aparte,
                        Conversation {
                            account: account.clone(),
                            jid: BareJid::from_str(&win_name).unwrap(),
                            kind: ConversationKind::Group,
                        },
                    );
                }
                if *user_request {
                    self.change_window(&win_name);
                }
            }
            Event::Win(window) => {
                if self.windows.contains(window) {
                    self.change_window(&window);
                } else {
                    aparte.log(format!("Unknown window {}", window));
                }
            }
            Event::WindowChange => {
                let (width, height) = termion::terminal_size().unwrap();
                self.root.measure(Some(width), Some(height));
                self.root.layout(1, 1);
                self.root.redraw();
            }
            Event::Key(key) => {
                match key {
                    Key::Char('\t') => {
                        let result = Rc::new(RefCell::new(None));

                        let (raw_buf, cursor, password) = {
                            self.root.event(&mut UIEvent::GetInput(Rc::clone(&result)));

                            let result = result.borrow_mut();
                            result.as_ref().unwrap().clone()
                        };

                        if password {
                            aparte.schedule(Event::Key(Key::Char('\t')));
                        } else {
                            let (account, conversation) = match &self.current_window {
                                Some(current_window) => {
                                    match self.conversations.get(current_window) {
                                        Some(conversation) => (
                                            Some(conversation.account.clone()),
                                            Some(conversation.jid.clone()),
                                        ),
                                        _ => (None, None),
                                    }
                                }
                                _ => (None, None),
                            };
                            aparte.schedule(Event::AutoComplete {
                                account,
                                conversation,
                                raw_buf,
                                cursor: cursor,
                            });
                        }
                    }
                    Key::Char('\n') => {
                        let result = Rc::new(RefCell::new(None));
                        // TODO avoid direct send to root, should go back to main event loop
                        self.root.event(&mut UIEvent::Validate(Rc::clone(&result)));

                        let result = result.borrow_mut();
                        let (raw_buf, password) = result.as_ref().unwrap();
                        let raw_buf = raw_buf.clone();
                        if *password {
                            let mut command = self.password_command.take().unwrap();
                            command.args.push(raw_buf.clone());
                            aparte.schedule(Event::Command(command));
                        } else if raw_buf.starts_with("/") {
                            match Command::try_from(&*raw_buf) {
                                Ok(command) => {
                                    aparte.schedule(Event::Command(command));
                                }
                                Err(error) => {
                                    aparte.schedule(Event::CommandError(error.to_string()));
                                }
                            }
                        } else if raw_buf.len() > 0 {
                            if let Some(current_window) = self.current_window.clone() {
                                if let Some(conversation) = self.conversations.get(&current_window)
                                {
                                    // TODO use current windows attached account
                                    let account = aparte.current_account().unwrap();
                                    let us = account.clone().into();
                                    match conversation.kind {
                                        ConversationKind::Chat => {
                                            let from: Jid = us;
                                            let to: Jid = conversation.jid.clone().into();
                                            let id = Uuid::new_v4();
                                            let timestamp = LocalTz::now().into();
                                            let message = Message::outgoing_chat(
                                                id.to_string(),
                                                timestamp,
                                                &from,
                                                &to,
                                                &raw_buf,
                                            );
                                            aparte.schedule(Event::SendMessage(
                                                account.clone(),
                                                message,
                                            ));
                                        }
                                        ConversationKind::Group => {
                                            let from: Jid = us;
                                            let to: Jid = conversation.jid.clone().into();
                                            let id = Uuid::new_v4();
                                            let timestamp = LocalTz::now().into();
                                            let message = Message::outgoing_groupchat(
                                                id.to_string(),
                                                timestamp,
                                                &from,
                                                &to,
                                                &raw_buf,
                                            );
                                            aparte.schedule(Event::SendMessage(
                                                account.clone(),
                                                message,
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Key::Alt('a') => {
                        if let Some(window) = self.unread_windows.pop_front() {
                            self.change_window(&window);
                        }
                    }
                    _ => {
                        aparte.schedule(Event::ResetCompletion);
                        self.root.event(&mut UIEvent::Core(Event::Key(key.clone())));
                    }
                }
            }
            Event::Completed(raw_buf, cursor) => {
                self.root.event(&mut UIEvent::Core(Event::Completed(
                    raw_buf.clone(),
                    cursor.clone(),
                )));
            }
            // Forward all unknown events
            event => self.root.event(&mut UIEvent::Core(event.clone())),
        }
    }
}

impl fmt::Display for UIPlugin {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Aparté UI")
    }
}

struct TermionEventStream {
    channel: mpsc::Receiver<Result<u8, IoError>>,
    waker: Arc<AtomicWaker>,
}

impl TermionEventStream {
    pub fn new() -> Self {
        let (send, recv) = mpsc::channel();
        let waker = Arc::new(AtomicWaker::new());

        let waker_for_tty = waker.clone();
        thread::spawn(move || {
            for i in get_tty().unwrap().bytes() {
                waker_for_tty.wake();
                if send.send(i).is_err() {
                    return;
                }
            }
        });

        Self {
            channel: recv,
            waker: waker,
        }
    }
}

struct IterWrapper<'a, T> {
    inner: &'a mut mpsc::Receiver<T>,
}

impl<'a, T> IterWrapper<'a, T> {
    fn new(inner: &'a mut mpsc::Receiver<T>) -> Self {
        Self { inner: inner }
    }
}

impl<'a, T> Iterator for IterWrapper<'a, T> {
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        match self.inner.try_recv() {
            Ok(e) => Some(e),
            Err(_) => None,
        }
    }
}

impl Stream for TermionEventStream {
    type Item = TermionEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let byte = match self.channel.try_recv() {
            Ok(Ok(byte)) => byte,
            Ok(Err(_)) => return Poll::Ready(None),
            Err(mpsc::TryRecvError::Empty) => {
                self.waker.register(cx.waker());
                return Poll::Pending;
            }
            Err(mpsc::TryRecvError::Disconnected) => return Poll::Ready(None),
        };

        let mut iter = IterWrapper::new(&mut self.channel);
        if let Ok(event) = termion_parse_event(byte, &mut iter) {
            Poll::Ready(Some(event))
        } else {
            self.waker.register(cx.waker());
            Poll::Pending
        }
    }
}

pub struct EventStream {
    inner: TermionEventStream,
}

impl EventStream {
    pub fn new() -> Self {
        Self {
            inner: TermionEventStream::new(),
        }
    }
}

impl Stream for EventStream {
    type Item = Event;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(TermionEvent::Key(key))) => {
                match key {
                    //Key::Alt('\x1b') => {
                    //    match Pin::new(&mut self.inner).poll_next(cx) {
                    //        Poll::Ready(Some(TermionEvent::Key(Key::Char('[')))) => {
                    //            match Pin::new(&mut self.inner).poll_next(cx) {
                    //                Poll::Ready(Some(TermionEvent::Key(Key::Char('C')))) => Poll::Pending,
                    //                Poll::Ready(Some(TermionEvent::Key(Key::Char('D')))) => Poll::Pending,
                    //                Poll::Ready(Some(TermionEvent::Key(_))) => Poll::Pending,
                    //                Poll::Ready(Some(TermionEvent::Key(_))) => Poll::Pending,
                    //                Poll::Ready(None) => Poll::Pending,
                    //                Poll::Pending => Poll::Pending,
                    //            }
                    //        },
                    //        _ => Poll::Pending,
                    //    };
                    //},
                    Key::Char(c) => Poll::Ready(Some(Event::Key(Key::Char(c)))),
                    Key::Backspace => Poll::Ready(Some(Event::Key(Key::Backspace))),
                    Key::Delete => Poll::Ready(Some(Event::Key(Key::Delete))),
                    Key::Home => Poll::Ready(Some(Event::Key(Key::Home))),
                    Key::End => Poll::Ready(Some(Event::Key(Key::End))),
                    Key::Up => Poll::Ready(Some(Event::Key(Key::Up))),
                    Key::Down => Poll::Ready(Some(Event::Key(Key::Down))),
                    Key::Left => Poll::Ready(Some(Event::Key(Key::Left))),
                    Key::Right => Poll::Ready(Some(Event::Key(Key::Right))),
                    Key::Ctrl(c) => Poll::Ready(Some(Event::Key(Key::Ctrl(c)))),
                    Key::Alt(c) => Poll::Ready(Some(Event::Key(Key::Alt(c)))),
                    _ => {
                        self.inner.waker.register(cx.waker());
                        Poll::Pending
                    }
                }
            }
            Poll::Ready(Some(TermionEvent::Mouse(_))) => {
                self.inner.waker.register(cx.waker());
                Poll::Pending
            }
            Poll::Ready(Some(TermionEvent::Unsupported(_))) => {
                self.inner.waker.register(cx.waker());
                Poll::Pending
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => {
                self.inner.waker.register(cx.waker());
                Poll::Pending
            }
        }
    }
}
