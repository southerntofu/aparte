#[macro_use]
extern crate log;
extern crate env_logger;
extern crate tokio;
extern crate tokio_xmpp;
extern crate xmpp_parsers;
extern crate rpassword;
extern crate futures;
extern crate minidom;

use std::convert::TryFrom;
use futures::{future, Future, Sink, Stream};
use tokio::runtime::current_thread::Runtime;
use tokio_xmpp::{Client, Packet};
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::presence::{Presence, Show as PresenceShow, Type as PresenceType};
use xmpp_parsers::carbons;
use xmpp_parsers::Element;
use xmpp_parsers::Jid;

mod plugins;

use plugins::{Plugin, PluginManager};

fn main_loop(client: Client, mut plugin_manager: PluginManager) {
    let mut rt = Runtime::new().unwrap();

    let (sink, stream) = client.split();

    let (mut tx, rx) = futures::unsync::mpsc::unbounded();
    rt.spawn(
        rx.forward(
            sink.sink_map_err(|_| panic!("Pipe"))
        ).map(|(rx, mut sink)| {
            drop(rx);
            let _ = sink.close();
        }).map_err(|e| {
            panic!("Send error: {:?}", e);
        })
    );

    let done = stream.for_each(move |event| {
        if event.is_online() {
            info!("We are now online");

            plugin_manager.on_connect(&mut tx);

            let mut presence = Presence::new(PresenceType::None);
            presence.show = Some(PresenceShow::Chat);
            presence.statuses.insert(String::from("en"), String::from("Echoing messages."));

            tx.start_send(Packet::Stanza(presence.into())).unwrap();
        } else if let Some(stanza) = event.into_stanza() {
            debug!("RECV: {}", String::from(&stanza));

            handle_stanza(stanza);
        }

        future::ok(())
    });

    match rt.block_on(done) {
        Ok(_) => (),
        Err(e) => {
            println!("Fatal: {}", e);
            ()
        }
    }
}

fn handle_stanza(stanza: Element) {
    if let Some(message) = Message::try_from(stanza).ok() {
        handle_message(message);
    }
}

fn handle_message(message: Message) {
    if let Some(from) = message.from {
        if let Some(ref body) = message.bodies.get("") {
            if message.type_ != MessageType::Error {
                match from {
                    Jid::Bare(jid) => println!("{}: {:?}", jid, body),
                    Jid::Full(jid) => println!("{}: {:?}", jid, body),
                }
            }
        } else {
            for payload in message.payloads {
                if let Some(received) = carbons::Received::try_from(payload).ok() {
                    if let Some(ref original) = received.forwarded.stanza {
                        if message.type_ != MessageType::Error {
                            if let Some(body) = original.bodies.get("") {
                                match from {
                                    Jid::Bare(jid) => println!("{}: {:?}", jid, body),
                                    Jid::Full(jid) => println!("{}: {:?}", jid, body),
                                }
                                break
                            }
                        }
                    }
                }
            }
        }
    }
}

fn main() {
    env_logger::init();

    let password = rpassword::read_password_from_tty(Some("Password: ")).unwrap();
    let client = Client::new("paul@fariello.eu", &password).unwrap();

    let mut plugin_manager = PluginManager::new();
    plugin_manager.add::<plugins::disco::Disco>(Box::new(plugins::disco::Disco::new())).unwrap();
    plugin_manager.add::<plugins::carbons::CarbonsPlugin>(Box::new(plugins::carbons::CarbonsPlugin::new())).unwrap();

    plugin_manager.init().unwrap();

    main_loop(client, plugin_manager)
}
