/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! An adapter for the built-in camera.


extern crate foxbox_taxonomy as taxonomy;

#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;

use taxonomy::channel::*;
use taxonomy::io::*;
use taxonomy::parse::*;
use taxonomy::services::{ AdapterId, Service };
use taxonomy::util::{ Id, Maybe };
use taxonomy::values::*;
use taxonomy::api::{ Operation, Error, InternalError, User };
use taxonomy::adapter::{ Adapter as AdapterT, AdapterManagerHandle, OpResult };

use std::fmt;
use std::process;
use std::sync::{ Arc, Mutex };

pub struct Html5Video {
    process: Mutex<Option<process::Child>>
}
impl Html5Video {
    fn new() -> Self {
        Html5Video {
            process: Mutex::new(None)
        }
    }

    /// Launch gstreamer in its own process, using `gst-launch`.
    // FIXME: Bring this in-process?
    pub fn start(&self, port: u32) -> Result<(), ()> {
        {
            if let Some(_) = *self.process.lock().unwrap() {
                return Err(unimplemented!()) // FIXME: It's already launched. Use some appropriate error.
            }
        }
        // The executable name depends on the platform.
        for name in &["gst-launch", "gst-launch-1.0"] {
            let executable = name.to_owned().clone();
            let mut command = process::Command::new(executable);
            let command = command
                // Capture the built-in cam.
                // FIXME: This requires gstreamer-plugins-bad
                // FIXME: This works on Mac. We'll need to adapt to other platforms.
                .arg("wrappercamerabinsrc mode=2")
                // Decode and reduce resolution. Future versions may accept the resolution as an arg.
                .arg("! videoconvert ! videoscale ! video/x-raw, width=320, height=240")
                // Reencode as ogg/theora, stream.
                .arg(format!("! theoraenc ! oggmux ! tcpserversink host=127.0.0.1 port={}", port));
            match command.spawn() {
                Ok(child) => {
                    *self.process.lock().unwrap() = Some(child);
                    return Ok(())
                }
                Err(err) => {
                    warn!("[sentry] Could not open html5 stream {:?}", err);
                }
            }
        }
        Err(())
    }
}

impl PartialEq for Html5Video {
    fn eq(&self, _: &Html5Video) -> bool {
        // Instances cannot be compared.
        false
    }
}

impl fmt::Debug for Html5Video {
    fn fmt(&self, format: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        match *self.process.lock().unwrap() {
            None => "Html5Video (paused)".fmt(format),
            Some(ref process) => format.write_fmt(format_args!("Html5Video (launched {})", process.id()))
        }
    }
}

/// Called when we don't have any client for the video anymore.
impl Drop for Html5Video {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.process.lock() {
            if let Some(ref mut child) = *guard {
                let _ = child.kill();
            }
        }
    }
}

impl Data for Html5Video {
    fn description() -> String {
        "Html5 video stream (for testing purposes only)".to_owned()
    }

    /// Parsing a Html5Video doesn't make sense.
    fn parse(_: Path, _: &JSON, _: &BinarySource) -> Result<Self, Error> where Self: Sized {
        unimplemented!() //FIXME: Replace me with a proper error
    }

    /// Serializing a Html5Video doesn't make sense.
    fn serialize(_: &Self, _: &BinaryTarget) -> Result<JSON, Error> where Self: Sized {
        unimplemented!() //FIXME: Replace me with a proper error"
    }
}

lazy_static! {
    static ref HTML5_VIDEO: Arc<Format> = Arc::new(Format::new::<Html5Video>());
}


pub struct Adapter {
    /// A channel used to get a new HTML5 stream.
    ///
    /// This channel returns a `Html5Video`.
    ///
    /// FIXME: Using HTML5 stream is very expensive, as we need to tunnel it through
    /// `knilxof.org` if the user is on a remote network. This channel will most
    /// likely be reserved for `debug` builds.
    id_channel_fetch_html5_stream: Id<Channel>,


    // A channel used to start/stop recording of the webcam to disk (TBD)
    //
    // Recording takes place on some kind of circular buffer, by splitting
    // the movie in 2-minute increments and erasing the oldest once we use
    // more than X bytes.
    // id_channel_control_recording: Id<Channel>,

    // A channel used to replay records.
    //
    // By default, records are replayed seamlessly from the start of the
    // circular buffer.
    //
    // For a first version, this channel returns a `Html5Video`.
    // id_channel_replay_records_html5_stream: Id<Channel>
}

static VERSION : [u32;4] = [0, 1, 0, 0];

impl AdapterT for Adapter {
    fn id(&self) -> Id<AdapterId> {
        Id::new("sentry@foxlink.mozilla.org")
    }
    fn name(&self) -> &str {
        "Built-in camera"
    }
    fn vendor(&self) -> &str {
        "Mozilla"
    }
    fn version(&self) -> &[u32;4] {
        &VERSION
    }
    fn fetch_values(&self, mut target: Vec<Id<Channel>>, _: User) -> OpResult<Value>
    {
        target.drain(..).map(|id| {
            if id != self.id_channel_fetch_html5_stream {
                return (id.clone(), Err(Error::OperationNotSupported(Operation::Watch, id)))
            }
            (id, Ok(Some(Value::new(Html5Video::new()))))
        }).collect()
    }
}

impl Adapter {
    pub fn init<T: AdapterManagerHandle + Send + Sync + 'static>(manager: &Arc<T>) -> Result<(), Error> {
        let id_channel_fetch_html5_stream = Id::new("sentry@foxlink.mozilla.org/livestream/html5");
        let adapter = Arc::new(Adapter {
            id_channel_fetch_html5_stream: id_channel_fetch_html5_stream.clone()
        });
        try!(manager.add_adapter(adapter.clone()));

        let adapter_id = adapter.id();
        let service_id = Id::new("service@foxlink.mozilla.org/livestream/html5");

        let service = Service::empty(&service_id, &adapter_id);
        try!(manager.add_service(service));

        let channel_live_stream_html5 = Channel {
            id: id_channel_fetch_html5_stream,
            adapter: adapter_id,
            service: service_id,
            supports_fetch: Some(Signature::returns(Maybe::Required(HTML5_VIDEO.clone()))),
            feature: Id::new("camera/live-stream-html5"),
            ..Channel::default()
        };
        try!(manager.add_channel(channel_live_stream_html5));
        Ok(())
    }
}
