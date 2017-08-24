// TeamSpeak3 Text to Speech Plugin
// Copyright © 2016  Sebastian Neubauer
//
// This program is free software; you can redistribute it and/or
// modify it under the terms of the GNU General Public License
// as published by the Free Software Foundation; either version 2
// of the License, or (at your option) any later version.

#[macro_use]
extern crate ts3plugin;
#[macro_use]
extern crate lazy_static;

use std::os::unix::io::{ AsRawFd, FromRawFd };
use std::process::*;
use std::sync::*;
use std::thread::{ self, JoinHandle };
use std::vec::Vec;

use ts3plugin::*;

/// Maximum number of texts read in parallel.
const MAX_TTS_PROCESSES: usize = 5;

/// A macro that compares to `Result` values.
/// It returns `true` if they are both ok and unequal,
/// otherwise `false` will be returned.
macro_rules! values_neq {
    ($a: expr, $b: expr) => {{
        // Evaluate the expressions only once
        let a = $a;
        let b = $b;
        a.is_ok() && b.is_ok() && a != b
    }};
}

struct TTSPlugin {
    /// The running threads. Before the plugin is ended, they have to be finished.
    /// The saved number is used for identifying threads because threads can't
    /// be compared.
    tts_threads: Arc<Mutex<(Vec<(JoinHandle<()>, usize)>, usize)>>,
}

/// Get the name or phonetic name of a server.
fn get_server_name(server: &Server) -> &str {
    server.get_phonetic_name().ok()
        .and_then(|s| if s.is_empty() { None } else { Some(s.as_str()) })
        .or_else(|| server.get_name().ok().map(|s| s.as_str()))
        .unwrap_or("unknown server")
}

/// Get the name or phonetic name of a channel.
fn get_channel_name<'a>(server: &'a Server, channel_id: ChannelId) -> &'a str {
    server.get_channel(channel_id)
        .and_then(|c| c.get_phonetic_name().ok()
            .and_then(|s| if s.is_empty() { None } else { Some(s) })
            .or_else(|| c.get_name().ok()))
        .map(|s| s.as_str())
        .unwrap_or("unknown channel")
}

fn intern_get_connection_name(connection: &Connection) -> &str {
    let result = connection.get_phonetic_name().ok()
        .and_then(|s| if s.is_empty() { None } else { Some(s.as_str()) })
        .or_else(|| connection.get_name().ok().map(|s| s.as_str()))
        .unwrap_or("unknown");
    // Take name until the first space
    result.find(' ').map_or(result, |i| result.split_at(i).0)
}

/// Get the name or phonetic name of a connection.
fn get_connection_name<'a>(server: &'a Server, connection_id: ConnectionId) -> &'a str {
    server.get_connection(connection_id).map_or("unknown",
        |connection| intern_get_connection_name(connection))
}

/// Get the name or phonetic name of an invoker.
/// THis uses the connection name if a connection is available. If not, it uses
/// the provided invoker name.
fn get_invoker_name<'a>(server: &'a Server, invoker: &'a Invoker) -> &'a str {
    let result = if server.get_connection(invoker.get_id()).is_some() {
            get_connection_name(server, invoker.get_id())
        } else {
            invoker.get_name().as_str()
        };
    // Take name until the first space
    result.find(' ').map_or(result, |i| result.split_at(i).0)
}

impl TTSPlugin {
    /// Read a message using espeak and aplay.
    fn tts<S: Into<String>>(&mut self, message: S) {
        let msg = message.into();
        TsApi::static_log_or_print(format!("Reading '{}'", msg), "TTSPlugin", LogLevel::Debug);

        // Don't spawn more than the max number of processes simultaneously
        let threads = self.tts_threads.clone();
        let mut threads_lock = self.tts_threads.lock().unwrap();
        if threads_lock.0.len() < MAX_TTS_PROCESSES {
            let thread_num = threads_lock.1.wrapping_add(1);
            threads_lock.1 = thread_num;
            // The moved variables should be threads and thread_num.
            threads_lock.0.push((thread::spawn(move || {
                // Pipe the output of espeak into aplay because espeak itself produces
                // stuttering sometimes.
                if let Ok(mut tts_process) = Command::new("espeak").arg("-ven+f2").arg("-s150")
                    .arg("--stdout").arg("--").arg(&msg).stdout(Stdio::piped()).spawn() {
                    // Write efficiently into aplay, unfortunately we need some
                    // unsafe code for that.
                    let handle = tts_process.stdout.as_ref().unwrap().as_raw_fd();
                    match Command::new("aplay").stdin(unsafe { Stdio::from_raw_fd(handle) }).spawn() {
                        Ok(mut play_process) => if let Err(error) = play_process.wait() {
                            TsApi::static_log_or_print(format!("Can't wait for aplay process because {}", error), "TTSPlugin", LogLevel::Error);
                        },
                        Err(error) => TsApi::static_log_or_print(format!("Can't start aplay process because {}", error), "TTSPlugin", LogLevel::Error)
                    }

                    if let Err(error) = tts_process.wait() {
                        TsApi::static_log_or_print(format!("Can't wait for espeak process because {}", error), "TTSPlugin", LogLevel::Error);
                    }
                } else {
                    TsApi::static_log_or_print(format!("Couldn't read '{}'", msg), "TTSPlugin", LogLevel::Error);
                }

                // Remove this thread from the list again
                let mut threads_lock = threads.lock().unwrap();
                if let Some(index) = threads_lock.0.iter().position(|&(_, num)| num == thread_num) {
                    threads_lock.0.remove(index);
                }
            }), thread_num));
        }
    }
}

impl Plugin for TTSPlugin {
    fn new(api: &mut TsApi) -> Result<Box<TTSPlugin>, InitError> {
        api.log_or_print("Starting", "TTSPlugin", LogLevel::Info);
        Ok(Box::new(TTSPlugin {
            tts_threads: Arc::new(Mutex::new((Vec::new(), 0))),
        }))
    }

    fn connect_status_change(&mut self, api: &mut TsApi, server_id: ServerId,
        status: ConnectStatus, _: Error) {
        if let Some(server) = api.get_server(server_id) {
            match status {
                ConnectStatus::Connected => self.tts(format!("Connected to {}", get_server_name(server))),
                ConnectStatus::Disconnected => self.tts(format!("Disconnected from {}", get_server_name(server))),
                _ => {}
            }
        }
    }

    fn server_stop(&mut self, api: &mut TsApi, server_id: ServerId, message: String) {
        if let Some(server) = api.get_server(server_id) {
            self.tts(format!("{} stopped{}", get_server_name(server),
                if message.is_empty() { String::new() } else { format!(" because {}", message) }));
        }
    }

    fn server_error(&mut self, api: &mut TsApi, _: ServerId, error: Error,
        message: String, _: String, extra_message: String) -> bool {
        api.log_or_print(format!("A server error occured: {} ({:?}; {})", message,
            error, extra_message), "TTSPlugin", LogLevel::Error);
        false
    }

    fn server_edited(&mut self, api: &mut TsApi, server_id: ServerId, invoker: Option<Invoker>) {
        if let Some(server) = api.get_server(server_id) {
            //TODO Compare changes
            if let Some(invoker) = invoker {
                self.tts(format!("{} edited the server", get_invoker_name(server, &invoker)));
            } else {
                self.tts("The server was edited");
            }
        }
    }

    fn connection_updated(&mut self, api: &mut TsApi, server_id: ServerId,
        connection_id: ConnectionId, old_connection: Option<Connection>, _: Invoker) {
        if let Some(server) = api.get_server(server_id) {
            // Compare changes
            if let Some(old) = old_connection {
                if let Some(new) = server.get_connection(connection_id) {
                    let name = get_connection_name(server, connection_id);

                    if values_neq!(new.get_name(), old.get_name()) {
                        // Don't use the phonetic name here
                        self.tts(format!("{} is now known as {}",
                            intern_get_connection_name(&old),
                            new.get_name().map(|s| s.as_str()).unwrap_or("unknown")));
                    }
                    if values_neq!(new.get_away(), old.get_away()) {
                        if new.get_away().unwrap() == AwayStatus::Zzz {
                            self.tts(format!("{} has gone{}", name,
                                new.get_away_message().ok().and_then(|s| if s.is_empty() {
                                    None
                                } else {
                                    Some(format!(" to {}", s))
                                }).unwrap_or(String::new())));
                        } else {
                            self.tts(format!("{} is back", name));
                        }
                    } else if new.get_away().map(|a| a == AwayStatus::Zzz).unwrap_or(false)
                        && values_neq!(new.get_away_message(), old.get_away_message()) {
                        self.tts(format!("{} has gone{}", name,
                            new.get_away_message().ok().and_then(|s| if s.is_empty() {
                                None
                            } else {
                                Some(format!(" to {}", s))
                            }).unwrap_or(String::new())));
                    }
                    if values_neq!(new.get_input_muted(), old.get_input_muted()) {
                        if new.get_input_muted().unwrap() == MuteInputStatus::Muted {
                            self.tts(format!("{} is muted", name));
                        } else {
                            self.tts(format!("{} is unmuted", name));
                        }
                    }
                    if values_neq!(new.get_output_muted(), old.get_output_muted()) {
                        if new.get_output_muted().unwrap() == MuteOutputStatus::Muted {
                            self.tts(format!("{} is deaf", name));
                        } else {
                            self.tts(format!("{} is listening", name));
                        }
                    }
                    if values_neq!(new.get_output_only_muted(), old.get_output_only_muted()) {
                        if new.get_output_only_muted().unwrap() == MuteOutputStatus::Muted {
                            self.tts(format!("{} is only deaf", name));
                        } else {
                            self.tts(format!("{} is listening again", name));
                        }
                    }
                    if values_neq!(new.get_input_hardware(), old.get_input_hardware()) {
                        if new.get_input_hardware().unwrap() == HardwareInputStatus::Disabled {
                            self.tts(format!("{} is silent", name));
                        } else {
                            self.tts(format!("{} can talk", name));
                        }
                    }
                    if values_neq!(new.get_output_hardware(), old.get_output_hardware()) {
                        if new.get_output_hardware().unwrap() == HardwareOutputStatus::Disabled {
                            self.tts(format!("{} is really deaf", name));
                        } else {
                            self.tts(format!("{} can listen", name));
                        }
                    }
                    if values_neq!(new.get_phonetic_name(), old.get_phonetic_name()) {
                        self.tts(format!("{} is now known as {}", intern_get_connection_name(&old), name));
                    }
                    if values_neq!(new.get_recording(), old.get_recording()) {
                        if new.get_recording().unwrap() {
                            self.tts(format!("{} starts recording", name));
                        } else {
                            self.tts(format!("{} stops recording", name));
                        }
                    }
                    // Not yet implemented in ts3plugin
                    let opt_new = new.get_optional_data();
                    let opt_old = old.get_optional_data();
                    if values_neq!(opt_new.get_description(), opt_old.get_description()) {
                        self.tts(format!("{} changed his description to {}", name, opt_new.get_description().unwrap()));
                    }
                    if values_neq!(opt_new.get_talker(), opt_old.get_talker()) {
                        if opt_new.get_talker().unwrap() {
                            self.tts(format!("{} is talker", name));
                        } else {
                            self.tts(format!("{} is no more talker", name));
                        }
                    }
                    if values_neq!(opt_new.get_priority_speaker(), opt_old.get_priority_speaker()) {
                        if opt_new.get_priority_speaker().unwrap() {
                            self.tts(format!("{} has priority", name));
                        } else {
                            self.tts(format!("{} has no more priority", name));
                        }
                    }
                    if opt_new.get_unread_messages() == Ok(true) {
                        self.tts(format!("Unread message from {}", name));
                    }
                }
            }
        }
    }

    fn connection_changed(&mut self, api: &mut TsApi, server_id: ServerId,
        connection_id: ConnectionId, connected: bool, _: String) {
        if let Some(server) = api.get_server(server_id) {
            // Ignore our own user
            if server.get_own_connection_id().ok().map_or(true, |c| c != connection_id) {
                let name = get_connection_name(server, connection_id);
                if !connected {
                    self.tts(format!("{} disconnected", name));
                } else {
                    let own_channel_id = server.get_own_connection_id().ok()
                        .and_then(|c| server.get_connection(c))
                        .and_then(|c| c.get_channel_id().ok());
                    let channel_id = server.get_connection(connection_id).and_then(|c| c.get_channel_id().ok());
                    if channel_id.is_some() && own_channel_id != channel_id {
                        self.tts(format!("{} connected to {}", name,
                            get_channel_name(server, channel_id.unwrap())));
                    } else {
                        self.tts(format!("{} connected", name));
                    }
                }
            }
        }
    }

    fn connection_move(&mut self, api: &mut TsApi, server_id: ServerId,
        connection_id: ConnectionId, old_channel_id: ChannelId,
        new_channel_id: ChannelId, visibility: Visibility) {
        if let Some(server) = api.get_server(server_id) {
            let connection = get_connection_name(server, connection_id);
            let new_channel = get_channel_name(server, new_channel_id);
            // Check if we are the client
            if Ok(connection_id) == server.get_own_connection_id() {
                self.tts(format!("Switched to {}", new_channel));
            } else {
                // Inform about changed visibility
                let vis = match visibility {
                    Visibility::Enter => " and appeared",
                    Visibility::Leave => " and disappeared",
                    _ => "",
                };

                // Check if the client joined our own channel
                let own_channel_id = server.get_own_connection_id().ok()
                    .and_then(|c| server.get_connection(c))
                    .and_then(|c| c.get_channel_id().ok());
                match own_channel_id {
                    Some(channel_id) if channel_id == old_channel_id => self.tts(format!("{} left to {}{}", connection, new_channel, vis)),
                    Some(channel_id) if channel_id == new_channel_id => self.tts(format!("{} joined{}", connection, vis)),
                    _ => self.tts(format!("{} switched to {}{}", connection, new_channel, vis)),
                }
            }
        }
    }

    fn connection_moved(&mut self, api: &mut TsApi, server_id: ServerId,
        connection_id: ConnectionId, old_channel_id: ChannelId,
        new_channel_id: ChannelId, visibility: Visibility, invoker: Invoker) {
        if let Some(server) = api.get_server(server_id) {
            let connection = get_connection_name(server, connection_id);
            let new_channel = get_channel_name(server, new_channel_id);
            let invoker_name = get_invoker_name(server, &invoker);
            // Check if we are the client
            if Ok(connection_id) == server.get_own_connection_id() {
                self.tts(format!("{} moved you to {}", invoker_name, new_channel));
            } else {
                // Check if the client joined our own channel
                let own_channel_id = server.get_own_connection_id().ok()
                    .and_then(|c| server.get_connection(c))
                    .and_then(|c| c.get_channel_id().ok());
                // Inform about changed visibility
                let vis = match visibility {
                    Visibility::Enter => " and appeared",
                    Visibility::Leave => " and disappeared",
                    _ => "",
                };

                match own_channel_id {
                    Some(channel_id) if channel_id == old_channel_id =>
                        self.tts(format!("{} moved {} out to {}{}", invoker_name,
                            connection, new_channel, vis)),
                    Some(channel_id) if channel_id == new_channel_id =>
                        self.tts(format!("{} moved {} in{}", invoker_name,
                            connection, vis)),
                    _ => self.tts(format!("{} moved {} to {}{}", invoker_name,
                        connection, new_channel, vis)),
                }
            }
        }
    }

    fn connection_timeout(&mut self, api: &mut TsApi, server_id: ServerId, connection_id: ConnectionId) {
        if let Some(server) = api.get_server(server_id) {
            if Ok(connection_id) == server.get_own_connection_id() {
                self.tts("Timed out");
            } else {
                let connection = get_connection_name(server, connection_id);
                self.tts(format!("{} timed out", connection));
            }
        }
    }

    fn channel_created(&mut self, api: &mut TsApi, server_id: ServerId,
        channel_id: ChannelId, invoker: Option<Invoker>) {
        if let Some(server) = api.get_server(server_id) {
            let name = if let Some(ref invoker) = invoker {
                get_invoker_name(server, invoker)
            } else {
                "The server"
            };
            self.tts(format!("{} created {}", name, get_channel_name(server, channel_id)));
        }
    }

    fn channel_deleted(&mut self, api: &mut TsApi, server_id: ServerId,
        channel_id: ChannelId, invoker: Option<Invoker>) {
        if let Some(server) = api.get_server(server_id) {
            let name = if let Some(ref invoker) = invoker {
                get_invoker_name(server, invoker)
            } else {
                "The server"
            };
            self.tts(format!("{} deleted {}", name, get_channel_name(server, channel_id)));
        }
    }

    fn channel_edited(&mut self, api: &mut TsApi, server_id: ServerId,
        channel_id: ChannelId, _: Option<Channel>, invoker: Invoker) {
        if let Some(server) = api.get_server(server_id) {
            //TODO Compare changes
            self.tts(format!("{} edited {}", get_invoker_name(server, &invoker),
                get_channel_name(server, channel_id)));
        }
    }

    fn channel_moved(&mut self, api: &mut TsApi, server_id: ServerId,
        channel_id: ChannelId, _: ChannelId, invoker: Option<Invoker>) {
        if let Some(server) = api.get_server(server_id) {
            let name = if let Some(ref invoker) = invoker {
                get_invoker_name(server, invoker)
            } else {
                "The server"
            };
            self.tts(format!("{} moved {}", name, get_channel_name(server, channel_id)));
        }
    }

    fn message(&mut self, api: &mut TsApi, server_id: ServerId, invoker: Invoker,
        _: MessageReceiver, message: String, ignored: bool) -> bool {
        if !ignored {
            if let Some(server) = api.get_server(server_id) {
                // Don't read our own messages
                if Ok(invoker.get_id()) != server.get_own_connection_id() {
                    if message.len() > 20 || message.contains("//") {
                        self.tts(format!("{} wrote a message", get_invoker_name(server,
                            &invoker)));
                    } else {
                        self.tts(format!("{} wrote {}", get_invoker_name(server, &invoker),
                            message));
                    }
                }
            }
        }
        false
    }

    fn poke(&mut self, api: &mut TsApi, server_id: ServerId, invoker: Invoker,
        message: String, ignored: bool) -> bool {
        if !ignored {
            if let Some(server) = api.get_server(server_id) {
                self.tts(format!("{} poked {}", get_invoker_name(server, &invoker),
                    message));
            }
        }
        false
    }

    fn channel_kick(&mut self, api: &mut TsApi, server_id: ServerId,
        connection_id: ConnectionId, _: ChannelId, new_channel_id: ChannelId,
        _: Visibility, invoker: Invoker, message: String) {
        if let Some(server) = api.get_server(server_id) {
            self.tts(format!("{} kicked {} to {}{}", get_invoker_name(server, &invoker),
                get_connection_name(server, connection_id),
                get_channel_name(server, new_channel_id),
                if message.is_empty() { String::new() } else { format!(" because {}", message) }));
        }
    }

    fn server_kick(&mut self, api: &mut TsApi, server_id: ServerId,
        connection_id: ConnectionId, invoker: Invoker, message: String) {
        if let Some(server) = api.get_server(server_id) {
            self.tts(format!("{} kicked {}{}", get_invoker_name(server, &invoker),
                get_connection_name(server, connection_id),
                if message.is_empty() { String::new() } else { format!(" because {}", message) }));
        }
    }

    fn server_ban(&mut self, api: &mut TsApi, server_id: ServerId,
        connection_id: ConnectionId, invoker: Invoker, message: String, _: u64) {
        if let Some(server) = api.get_server(server_id) {
            self.tts(format!("{} banned {}{}", get_invoker_name(server, &invoker),
                get_connection_name(server, connection_id),
                if message.is_empty() { String::new() } else { format!(" because {}", message) }));
        }
    }

    fn avatar_changed(&mut self, api: &mut TsApi, server_id: ServerId,
        connection_id: ConnectionId, path: Option<String>) {
        if let Some(server) = api.get_server(server_id) {
            self.tts(format!("{} {} his avatar", get_connection_name(server, connection_id),
                if path.is_none() { "removed" } else { "changed" }));
        }
    }

    // Called at each channel switch
    /*fn connection_channel_group_changed(&mut self, api: &mut TsApi, server_id: ServerId,
        connection_id: ConnectionId, _: ChannelGroupId, _: ChannelId,
        _: Invoker) {
        if let Some(server) = api.get_server(server_id) {
            self.tts(format!("{} changed channel group", get_connection_name(server, connection_id)));
        }
    }*/

    fn connection_server_group_added(&mut self, api: &mut TsApi, server_id: ServerId,
        connection: Invoker, _: ServerGroupId, invoker: Invoker) {
        if let Some(server) = api.get_server(server_id) {
            self.tts(format!("{} added {} to a group", get_invoker_name(server, &invoker),
                get_invoker_name(server, &connection)));
        }
    }

    fn connection_server_group_removed(&mut self, api: &mut TsApi, server_id: ServerId,
        connection: Invoker, _: ServerGroupId, invoker: Invoker) {
        if let Some(server) = api.get_server(server_id) {
            self.tts(format!("{} removed {} from a group", get_invoker_name(server, &invoker),
                get_invoker_name(server, &connection)));
        }
    }

    fn permission_error(&mut self, _: &mut TsApi, _: ServerId,
        _: PermissionId, _: Error, message: String, _: String) -> bool {
        self.tts(format!("Denied {}", message));
        false
    }

    fn shutdown(&mut self, api: &mut TsApi) {
        api.log_or_print("Shutdown", "TTSPlugin", LogLevel::Info);
        // Wait for tts threads to finish
        // Move JoinHandles out of the list, so we don't block the mutex
        let handles: Vec<JoinHandle<()>> = {
            let mut threads_lock = self.tts_threads.lock().unwrap();
            // A helper variable is needed here because we have the lock only temporary
            let h: Vec<JoinHandle<()>> = threads_lock.0.drain(..).map(|(handle, _)| handle).collect();
            h
        };
        for h in handles {
            // We get an error if the thread panicked, just ignore it, if it
            // panicks, we should get output anyway.
            h.join().ok();
        }
    }
}

create_plugin!("Text to Speech", "0.2.0", "Seebi", "A text to speech plugin.",
    ConfigureOffer::No, false, TTSPlugin);
