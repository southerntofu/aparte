/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */
use std::fmt;
use std::str::FromStr;
use std::collections::HashMap;
use uuid::Uuid;
use xmpp_parsers::Element;
use xmpp_parsers::pubsub::{PubSub, pubsub, pubsub::Items, Item, ItemId, pubsub::Publish, pubsub::PublishOptions, NodeName};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::ns;
use xmpp_parsers::{Jid, BareJid};
use xmpp_parsers::data_forms::{DataForm, DataFormType, Field, FieldType};
use xmpp_parsers::bookmarks2::{Conference, Autojoin};

use crate::core::{Plugin, Aparte, Event};
use crate::command::{Command, CommandParser};
use crate::plugins::disco;

command_def!(bookmark_add,
r#"/bookmark add <bookmark> <conference> [autojoin=on|off]

    bookmark    The bookmark friendly name
    conference  The conference room jid
    autojoin    Wether the conference room should be automatically joined on startup

Description:
    Add a bookmark

Examples:
    /bookmark add aparte aparte@conference.fariello.eu
    /bookmark add aparte aparte@conference.fariello.eu/mynick
    /bookmark add aparte aparte@conference.fariello.eu/mynick autojoin=on
"#,
{
    name: String,
    conference: Jid,
    autojoin: Option<bool>
},
|aparte, _command| {
    let add = {
        let bookmarks = aparte.get_plugin::<BookmarksPlugin>().unwrap();
        let nick = match conference.clone() {
            Jid::Bare(_room) => None,
            Jid::Full(room) => Some(room.resource),
        };
        let autojoin = match autojoin {
            None => false,
            Some(autojoin) => autojoin,
        };
        bookmarks.add(name, conference.into(), nick, autojoin)
    };
    aparte.send(add);
    Ok(())
});

command_def!(bookmark_del,
r#"/bookmark del <bookmark>

    bookmark    The bookmark friendly name

Description:
    Delete a bookmark

Examples:
    /bookmark del aparte
"#,
{
    _name: String
},
|_aparte, _command| {
    Ok(())
});

command_def!(bookmark_edit,
r#"/bookmark edit <bookmark> [<conference>] [autojoin=on|off]

    bookmark    The bookmark friendly name
    conference  The conference room jid
    autojoin    Wether the conference room should be automatically joined on startup

Description:
    Edit a bookmark

Examples:
    /bookmark edit aparte autojoin=on
    /bookmark edit aparte aparte@conference.fariello.eu
    /bookmark edit aparte aparte@conference.fariello.eu autojoin=off
"#,
{
    _name: String
},
|_aparte, _command| {
    Ok(())
});

command_def!(bookmark,
r#"/bookmark add|del|edit"#,
{
    action: Command = {
        children: {
            "add": bookmark_add,
            "del": bookmark_del,
            "edit": bookmark_edit,
        }
    },
});

pub struct BookmarksPlugin {
}

impl BookmarksPlugin {
    fn retreive(&self) -> Element {
        let id = Uuid::new_v4().to_hyphenated().to_string();
        let items = Items {
            max_items: None,
            node: NodeName(String::from(ns::BOOKMARKS2)),
            subid: None,
            items: vec![],
        };
        let pubsub = PubSub::Items(items);
        let iq = Iq::from_get(id, pubsub);
        iq.into()
    }

    fn add(&self, name: String, conference: BareJid, nick: Option<String>, autojoin: bool) -> Element {
        let id = Uuid::new_v4().to_hyphenated().to_string();
        let item = Item {
            id: Some(ItemId(conference.into())),
            payload: Some(Conference {
                autojoin: match autojoin {
                    true => Autojoin::True,
                    false => Autojoin::False,
                },
                name: Some(name),
                nick: nick,
                password: None
            }.into()),
            publisher: None,
        };
        let publish = Publish {
            node: NodeName(String::from(ns::BOOKMARKS2)),
            items: vec![pubsub::Item(item)],
        };
        let options = PublishOptions {
            form: Some(DataForm {
                type_: DataFormType::Submit,
                form_type: Some(String::from("http://jabber.org/protocol/pubsub#publish-options")),
                title: None,
                instructions: None,
                fields: vec![Field {
                    var: String::from("pubsub#persist_items"),
                    type_: FieldType::Boolean,
                    label: None,
                    required: false,
                    media: vec![],
                    options: vec![],
                    values: vec![String::from("true")],
                },
                Field {
                    var: String::from("pubsub#access_model"),
                    type_: FieldType::ListSingle,
                    label: None,
                    required: false,
                    media: vec![],
                    options: vec![],
                    values: vec![String::from("whitelist")],
                }],
            })
        };
        let pubsub = PubSub::Publish{publish: publish, publish_options: Some(options)};
        let iq = Iq::from_set(id, pubsub);
        iq.into()
    }
}

impl Plugin for BookmarksPlugin {
    fn new() -> BookmarksPlugin {
        BookmarksPlugin { }
    }

    fn init(&mut self, aparte: &mut Aparte) -> Result<(), ()> {
        aparte.add_command(bookmark::new());
        let mut disco = aparte.get_plugin_mut::<disco::Disco>().unwrap();
        disco.add_feature(ns::BOOKMARKS2)
    }

    fn on_event(&mut self, aparte: &mut Aparte, event: &Event) {
        match event {
            Event::Connected(_jid) => {
                aparte.send(self.retreive())
            },
            _ => {},
        }
    }
}

impl fmt::Display for BookmarksPlugin {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "XEP-0402: PEP Native Bookmarks")
    }
}
