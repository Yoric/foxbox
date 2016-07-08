/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! An adapter for the built-in camera.

extern crate foxbox_core;
extern crate foxbox_taxonomy;

extern crate gst;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;
extern crate rand;

use foxbox_core::traits::Controller;

use foxbox_taxonomy::channel::*;
use foxbox_taxonomy::io::*;
use foxbox_taxonomy::parse::*;
use foxbox_taxonomy::services::{ AdapterId, Service };
use foxbox_taxonomy::util::{ Id, Maybe };
use foxbox_taxonomy::values::*;
use foxbox_taxonomy::api::{ Operation, Error, User };
use foxbox_taxonomy::adapter::{ Adapter as AdapterT, AdapterManagerHandle, OpResult, ResultMap };

use std::collections::HashMap;
use std::fmt;
use std::path;
use std::sync::{ Arc, Mutex };
use std::sync::mpsc::channel;
use std::thread;

/// Ensure that GStreamer is initialized.
///
/// This function is idempotent.
fn gst_ensure_initialized() {
    *GST_INITIALIZED;
}

#[derive(Clone)]
struct Html5Video {
    port: u16,
}
impl Html5Video {
    /// Start a stream, immediately.

    // FIXME: For the time being, we have no way of closing the stream when there are no clients.
    // FIXME: To implement this, we may need some kind of watchdog based e.g. on polling netstat.
    fn new() -> Result<Html5Video, Error> {
        gst_ensure_initialized();

        // Capture the built-in cam. This requires gstreamer-plugins-bad. There may be a
        // better solution.
        // FIXME: This works on Mac. We'll need to adapt to other platforms.
        let spec_capture = "wrappercamerabinsrc mode=2";

        // Decode and reduce resolution. Future versions may accept the resolution as an arg.
        let spec_decode = "videoconvert ! videoscale ! video/x-raw, width=320, height=240";

        // Reencode as ogg/theora.
        // FIXME: This is CPU expensive. There may be a less expensive solution.
        let spec_reencode = "theoraenc ! oggmux";

        // Find a port for streaming.
        let spec_stream = "tcpserversink host=127.0.0.1 port=0 name=server";

        let spec = format!("{} ! {} ! {} ! {}", spec_capture, spec_decode, spec_reencode, spec_stream);

        info!("[sentry] Preparing pipeline {}", spec);
        let mut pipeline = gst::Pipeline::new_from_str(&spec).unwrap();

        info!("[sentry] Extracting bus and main loop");
        let mut bus = pipeline.bus().expect("[sentry] Couldn't extract bus from pipeline");
        let mut mainloop = gst::MainLoop::new(); // FIXME: Do we really need several loops?

        // Delegate to a thread, but wait until initialization is complete to return.
        let (tx, rx) = channel();

        thread::spawn(move || {
            info!("[sentry] spawning main loop");
            mainloop.spawn();

            info!("[sentry] starting pipeline");
            pipeline.play();

            // Normally, by now, a port has been allocated.
            let server = pipeline.get_by_name("server").unwrap();
            let port : u16 = server.get("current-port");
            info!("[sentry] now streaming on port {}", port);
            let _ = tx.send(port);

            info!("[sentry] playing messages");
            for message in bus.receiver().iter() {
                match message.parse() {
                    gst::Message::StateChangedParsed { ref old, ref new, .. } => {
                        info!("[sentry] element `{}` changed from {:?} to {:?}", message.src_name(), old, new);
                    }
                    gst::Message::ErrorParsed {ref error, ..} => {
                        info!("[sentry] error msg from element `{}`: {}, quitting", message.src_name(), error.message());
                        break;
                    }
                    gst::Message::Eos(_) => {
                        info!("[sentry] eos received, stopping loop and pipeline");
                        break;
                    }
                    _ => {
                        info!("[sentry] msg of type `{}` from element `{}`", message.type_name(), message.src_name());
                    }
                }
            }
            mainloop.quit();
        });

        let port = rx.recv().unwrap();
        Ok(Html5Video {
            port: port
        })
    }
}

impl Data for Html5Video {
    fn description() -> String {
        "Html5 video stream (for testing purposes only)".to_owned()
    }
    /// DirectPortAccess values cannot be parsed.
    fn parse(path: Path, _: &JSON, _: &BinarySource) -> Result<Self, Error> where Self: Sized {
        Err(Error::ParseError(ParseError::type_error(&<Self as Data>::description(), &path, "A value that supports deserialization")))
    }

    /// DirectPortAccess values are serialized as their port.
    fn serialize(source: &Self, _: &BinaryTarget) -> Result<JSON, Error> where Self: Sized {
        Ok(vec![("port", JSON::U64(source.port as u64))].to_json())
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
        format.write_fmt(format_args!("Html5Video (port {})", self.port))
    }
}

lazy_static! {
    static ref HTML5_VIDEO: Arc<Format> = Arc::new(Format::new::<Html5Video>());
    static ref GST_INITIALIZED: () = gst::init();
}


pub struct Adapter {
    /// The directory in which to store files.
    storage_root: path::PathBuf,

    /// A channel used to get a new HTML5 stream.
    ///
    /// This channel returns a `Html5Video`.
    ///
    /// FIXME: Using HTML5 stream is very expensive, as we need to tunnel it through
    /// `knilxof.org` if the user is on a remote network. This channel will most
    /// likely be reserved for `debug` builds.
    id_channel_fetch_html5_stream: Id<Channel>,
    livestreamer: Mutex<Option<Html5Video>>,

    // A channel used to start/stop recording of the webcam to disk (TBD)
    //
    // Recording takes place on some kind of circular buffer, by splitting
    // the movie in 2-minute increments and erasing the oldest once we use
    // more than X bytes.
    id_channel_control_recording: Id<Channel>,
    recorder: Mutex<Option<Arc<Mutex<gst::Pipeline>>>>,

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
            if id == self.id_channel_fetch_html5_stream {
                let mut lock = self.livestreamer.lock().unwrap();
                if let Some(ref video) = *lock {
                    return (id, Ok(Some(Value::new((*video).clone()))))
                }
                match Html5Video::new() {
                    Ok(video) => {
                        *lock = Some(video.clone());
                        (id, Ok(Some(Value::new(video))))
                    },
                    Err(err) => (id, Err(err))
                }
            } else if id == self.id_channel_control_recording {
                match *self.recorder.lock().unwrap() {
                    None => (id, Ok(Some(Value::new(OnOff::Off)))),
                    Some(_) => (id, Ok(Some(Value::new(OnOff::On)))),
                }
            } else {
                (id.clone(), Err(Error::OperationNotSupported(Operation::Fetch, id)))
            }
        }).collect()
    }
    fn send_values(&self, mut target: HashMap<Id<Channel>, Value>, _: User) -> ResultMap<Id<Channel>, (), Error>
    {
        target.drain().map(|(id, value)| {
            if id == self.id_channel_control_recording {
                match value.cast::<OnOff>() {
                    Err(err) => (id, Err(err)),
                    Ok(&OnOff::On) => {
                        let mut lock = self.recorder.lock().unwrap();
                        if let Some(_) = *lock {
                            return (id, Ok(())) // Already recording
                        }
                        gst_ensure_initialized();

                        // Capture the built-in cam. This requires gstreamer-plugins-bad. There may be a
                        // better solution.
                        // FIXME: This works on Mac. We'll need to adapt to other platforms.
                        let spec_capture = "wrappercamerabinsrc mode=2";

                        // Decode and reduce resolution. Future versions may accept the resolution as an arg.
                        let spec_decode = "videoconvert ! videoscale ! video/x-raw, width=320, height=240";

                        // Reencode as ogg/theora.
                        // FIXME: This is CPU expensive. There may be a less expensive solution.
                        let spec_reencode = "theoraenc ! oggmux";

                        // Store to disk.
                        // FIXME: We should store to a bounded buffer.
                        let dest = self.storage_root.join(&path::Path::new("record.ogg"));
                        let spec_stream = &format!("filesink location=\"{}\"", dest.to_str().unwrap());
                        let spec = format!("{} ! {} ! {} ! {}", spec_capture, spec_decode, spec_reencode, spec_stream);

                        info!("[sentry] Preparing pipeline {}", spec);
                        let pipeline = Arc::new(Mutex::new(gst::Pipeline::new_from_str(&spec).unwrap()));

                        info!("[sentry] Extracting bus and main loop");
                        let mut bus = pipeline.lock().unwrap().bus().expect("[sentry] Couldn't extract bus from pipeline");
                        let mut mainloop = gst::MainLoop::new(); // FIXME: Do we really need several loops?
                        *lock = Some(pipeline.clone());
                        thread::spawn(move || {
                            info!("[sentry] spawning main loop");
                            mainloop.spawn();

                            info!("[sentry] starting pipeline");
                            pipeline.lock().unwrap().play();

                            info!("[sentry] playing messages");
                            for message in bus.receiver().iter() {
                                match message.parse() {
                                    gst::Message::StateChangedParsed { ref old, ref new, .. } => {
                                        info!("[sentry] element `{}` changed from {:?} to {:?}", message.src_name(), old, new);
                                    }
                                    gst::Message::ErrorParsed {ref error, ..} => {
                                        info!("[sentry] error msg from element `{}`: {}, quitting", message.src_name(), error.message());
                                        break;
                                    }
                                    gst::Message::Eos(_) => {
                                        info!("[sentry] eos received, stopping loop and pipeline");
                                        break;
                                    }
                                    _ => {
                                        info!("[sentry] msg of type `{}` from element `{}`", message.type_name(), message.src_name());
                                    }
                                }
                            }
                            mainloop.quit();
                        });
                        (id, Ok(()))
                    }
                    Ok(&OnOff::Off) => {
                        match self.recorder.lock().unwrap().take() {
                            Some(pipeline) => {
                                info!("[sentry] stopping record");
                                let _ = pipeline.lock().unwrap().set_null_state();
                                info!("[sentry] record stopped");
                                (id, Ok(()))
                            }
                            None => (id, Ok(())) // Already stopped
                        }
                    }
                }
            } else {
                (id.clone(), Err(Error::OperationNotSupported(Operation::Send, id)))
            }
        }).collect()
    }
}

impl Adapter {
    pub fn init<T, C>(manager: &Arc<T>, controller: &C) -> Result<(), Error>
        where
            T: AdapterManagerHandle + Send + Sync + 'static,
            C: Controller
    {
        use std::fs;
        let storage_root = path::PathBuf::from(controller.get_profile().path_for("sentry"));
        if let Err(err) = fs::create_dir_all(&storage_root) {
            warn!("[sentry] Could not create storage directory: {}", err);
        }

        let id_channel_fetch_html5_stream = Id::new("sentry@foxlink.mozilla.org/livestream/html5");
        let id_channel_control_recording = Id::new("sentry@foxlink.mozilla.org/record/ogg");
        let adapter = Arc::new(Adapter {
            storage_root: storage_root,

            id_channel_fetch_html5_stream: id_channel_fetch_html5_stream.clone(),
            livestreamer: Mutex::new(None),

            id_channel_control_recording: id_channel_control_recording.clone(),
            recorder: Mutex::new(None),
        });
        try!(manager.add_adapter(adapter.clone()));

        let adapter_id = adapter.id();
        let service_id = Id::new("service@foxlink.mozilla.org/livestream/html5");
        try!(manager.add_service(Service {
            id: service_id.clone(),
            adapter: adapter_id.clone(),
            ..Service::default()
        }));

        let channel_live_stream_html5 = Channel {
            id: id_channel_fetch_html5_stream,
            adapter: adapter_id.clone(),
            service: service_id.clone(),
            supports_fetch: Some(Signature::returns(Maybe::Required(HTML5_VIDEO.clone()))),
            feature: Id::new("camera/live-stream-html5"),
            ..Channel::default()
        };
        try!(manager.add_channel(channel_live_stream_html5));

        let channel_control_recording = Channel {
            id: id_channel_control_recording,
            adapter: adapter_id.clone(),
            service: service_id.clone(),
            supports_fetch: Some(Signature::returns(Maybe::Required(format::ON_OFF.clone()))),
            supports_send: Some(Signature::accepts(Maybe::Required(format::ON_OFF.clone()))),
            feature: Id::new("camera/is-recording-ogg"),
            ..Channel::default()
        };
        try!(manager.add_channel(channel_control_recording));

        Ok(())
    }
}