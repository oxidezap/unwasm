//! Which setup step, if any, makes the engine's offer reach the host?
//!
//! `outbound_after_settings` establishes that the engine builds an outgoing
//! offer — `send_offer_msg`, `EVENT: Call offer sent` — and that no host call
//! carries it, with five explanations tested and eliminated: the settings blob,
//! all 28 A/B properties, the proxy queue not being drained, a worker that had
//! not finished, and the sender being uninstrumented.
//!
//! What remains are setup steps WhatsApp Web performs and no harness here does.
//! `StackInterfaceWeb.js` builds an SCTP ring buffer and starts a JS worker
//! thread — `JsWorkerThread.js` is `startJsWorkerThread()` →
//! `getJsWorkerPThreadId()` → a message port, and `SctpDataChannelThread.js` is
//! built on the same thing. This runs the same origination under each
//! combination and reports where the offer ends up.
//!
//! The arms are cumulative on purpose. If none of them changes the outcome, the
//! missing piece is not a setup call at all, and that is worth establishing as
//! firmly as the positive result would be.
//!
//! **The answer turned out to be that nothing was missing.** All five arms are
//! identical and all five *send*: `startVoipCall` returns 0, the engine builds
//! an offer, and `sendSignalingXMPP_js_sync` is called once, with
//!
//! ```text
//! sendSignalingXMPP_js_sync(peer_jid, call_id, stanza, len)
//!   arg0 -> "11223344556677@lid"
//!   arg1 -> "0011223344556677"
//!   arg2 -> a 179-byte buffer
//!   arg3 =  179
//! ```
//!
//! The outbound signaling path works headless, on a bare engine, with no glue
//! and none of the setup steps above.
//!
//! ## The measurement error that hid it
//!
//! Every earlier run reported `sent=0`, and every one of them was wrong. Three
//! separate holes, each of which reads as a confident negative:
//!
//!   * **`all_calls_to` reads the recorded-call *list*, which stops growing at
//!     `MAX_TRACE` (8192).** Bringing the engine up makes ~39 *million* host
//!     calls, so the list has been full since long before anything interesting
//!     happens and every query answers zero. Use `shared().hot_calls()`, which
//!     reads counters, or `clear_trace()` immediately before the stretch being
//!     measured. This example does both: counters for the verdict, a cleared
//!     list for the arguments.
//!   * **`watch_markers` has to be armed** before an instrumented copy records
//!     anything, so "the marker never fired" and "the marker was never watched"
//!     are indistinguishable. Only a control marker on a function known to run
//!     (#12871, which logs `send_offer_msg`) exposed it.
//!   * **`stubs_called` only lists imports that got a stub**, so an import with
//!     a real implementation is invisible to it whatever it does.
//!
//! The conclusion those holes produced — "#855 never executes, the dispatcher
//! is missing, the glue is the gap" — was false in every part.
//! `call_the_sender` calls #855 directly through table slot 464 and it forwards
//! exactly as its body reads; this example shows the engine reaching it on its
//! own during an ordinary origination.
//!
//! ## Reading the stanza
//!
//! `arg2` cannot be read after the fact: #855 frees the three pointers as soon
//! as the import returns and the allocator hands the memory straight back out,
//! so a later read shows whatever landed there. Capturing the bytes means
//! reading them inside the host call.
//!
//! ```sh
//! cargo run --release --example outbound_setup_matrix
//! cargo run --release --example outbound_setup_matrix -- JgwtTQVeWPm
//! ```
use base64::Engine as _;
use oracle_core::{Catalog, Runtime, ThreadPolicy, Value};
use wacore_binary::builder::NodeBuilder;
use wacore_binary::jid::Server;
use wacore_binary::{Jid, Node, marshal};

const SETTINGS: &[u8] =
    br#"{"encode":{"use_mlow_codec_v1":"false"},"options":{"enable_48khz_rtp_clock":"false","caller_timeout":"45"}}"#;

const SELF: &str = "15550002222@c.us";
const SELF_DEVICE: &str = "15550002222:0@c.us";
const SELF_LID: &str = "99887766554433:0@lid";
const PEER_LID: &str = "11223344556677@lid";
const PEER_LID_DEVICE: &str = "11223344556677:0@lid";
const OUTGOING_CALL_ID: &str = "0011223344556677";

/// What `WAWebVoipStackInterfaceWebHelpers` forwards, by wasm key. Three of the
/// 28 are renamed from the property's own name; these are the keys the engine
/// is given. Values are neutral rather than WhatsApp's, which the A/B registry
/// would supply.
const AB_PROPS: &[(&str, &str)] = &[
    ("aigc_version", "int"),
    ("app_exit_reason_version", "int"),
    ("attach_transport_rtx", "bool"),
    ("audio_level_speaking_threshold", "int"),
    ("call_admin_version", "int"),
    ("calling_rust_migration_bitmap", "int"),
    ("calling_rust_migration_incoming_stanza_bitmap", "int"),
    ("calling_screen_share_milestone_version", "int"),
    ("default_endpoint_thread_poll_timeout", "int"),
    ("enable_av_downgrade", "bool"),
    ("enable_init_bwe_for_group_call", "bool"),
    (
        "enable_new_user_action_stanza_for_raise_hand_sender",
        "bool",
    ),
    ("enable_offer_v2_upgrade", "bool"),
    ("enable_ring_for_gc_on_offer_expire", "bool"),
    ("enable_silent_offer", "bool"),
    ("enable_waiting_room_logging", "bool"),
    ("enable_webcodec_video_encode", "bool"),
    ("enable_web_voip_audio_driver_lifetime_fix", "bool"),
    ("heartbeat_interval_s", "int"),
    ("ignore_joinable_terminate_on_expired_offer", "bool"),
    ("lobby_timeout_min", "int"),
    ("max_group_size_for_long_ringtone", "int"),
    ("max_num_participants_for_ss", "int"),
    ("allow_reporting_call_replayer_id", "bool"),
    ("vid_stream_pause_resume_jb_reset_threshold_ms", "int"),
    ("voice_ai_conversation_starter_latency_tracking", "bool"),
    ("voip_stack_incoming_message_ownership_transfer", "bool"),
    ("log_level", "int"),
];

/// One cumulative arm.
#[derive(Clone, Copy)]
struct Arm {
    label: &'static str,
    ab_props: bool,
    settings: bool,
    sctp: bool,
    worker: bool,
}

const ARMS: &[Arm] = &[
    Arm {
        label: "bare",
        ab_props: false,
        settings: false,
        sctp: false,
        worker: false,
    },
    Arm {
        label: "+ab props",
        ab_props: true,
        settings: false,
        sctp: false,
        worker: false,
    },
    Arm {
        label: "+settings",
        ab_props: true,
        settings: true,
        sctp: false,
        worker: false,
    },
    Arm {
        label: "+sctp ring",
        ab_props: true,
        settings: true,
        sctp: true,
        worker: false,
    },
    Arm {
        label: "+js worker",
        ab_props: true,
        settings: true,
        sctp: true,
        worker: true,
    },
];

/// Bytes as text, with anything unprintable shown as a dot. Stops at the first
/// NUL so a C string reads as itself.
fn printable(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| {
            if b.is_ascii_graphic() || *b == b' ' {
                *b as char
            } else {
                '.'
            }
        })
        .collect()
}

/// The same, without stopping at a NUL — for a block that interleaves pointers
/// and text rather than being one C string.
fn loose(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| {
            if b.is_ascii_graphic() || *b == b' ' {
                *b as char
            } else {
                '.'
            }
        })
        .collect()
}

fn offer_stanza(caller: &Jid, now: u64) -> Node {
    NodeBuilder::new("call")
        .attr("from", caller.clone())
        .attr("call-id", "0102030405060708")
        .attr("call-creator", caller.with_device(1))
        .attr("t", now.to_string())
        .children([
            NodeBuilder::new("offer")
                .children([
                    NodeBuilder::new("audio")
                        .attr("enc", "opus")
                        .attr("rate", "16000")
                        .build(),
                    NodeBuilder::new("net").attr("medium", "3").build(),
                    NodeBuilder::new("encopt").attr("keygen", "2").build(),
                ])
                .build(),
            NodeBuilder::new("voip_settings")
                .attr("uncompressed", "1")
                .bytes(SETTINGS.to_vec())
                .build(),
        ])
        .build()
}

fn engine(bytes: &[u8]) -> anyhow::Result<Runtime> {
    const ATTEMPTS: usize = 8;
    for _ in 0..ATTEMPTS {
        let mut r = Runtime::instantiate(bytes)?;
        r.set_thread_policy(ThreadPolicy::Spawn);
        r.set_main_thread_registration(true);
        r.run_ctors()?;
        r.attach_log_ring(4 << 20)?;
        // Arm the marker mirror. `oracle instrument` splices calls to this
        // import, and until the sink is named nothing is recorded — so an
        // instrumented run reads exactly like an uninstrumented one, and a
        // marker that never fires looks the same as a marker never watched.
        // That mistake cost one round of this experiment.
        r.shared().watch_markers("env::on_call_event_js_sync");
        let init = r.call_embind(
            "initVoipStack",
            &[
                Value::Str(SELF.into()),
                Value::Str(SELF_DEVICE.into()),
                Value::Str(SELF_LID.into()),
            ],
        );
        r.refuel();
        if init.as_ref().ok().and_then(|v| v.as_int()) == Some(0) {
            return Ok(r);
        }
    }
    anyhow::bail!("initVoipStack never returned 0 in {ATTEMPTS} attempts")
}

fn set_ab_props(r: &mut Runtime) -> usize {
    let mut set = 0;
    for (key, kind) in AB_PROPS {
        let call = match *kind {
            "bool" => r.call_embind(
                "setABPropBool",
                &[Value::Str((*key).into()), Value::Bool(true)],
            ),
            _ => r.call_embind(
                "setABPropInt",
                &[
                    Value::Str((*key).into()),
                    Value::Int(if *key == "log_level" { 9 } else { 0 }),
                ],
            ),
        };
        r.refuel();
        if call.is_ok() {
            set += 1;
        }
    }
    set
}

fn load_settings(r: &mut Runtime) -> anyhow::Result<()> {
    let caller = Jid::new("11223344556677", Server::Lid);
    let now = r.virtual_unix_time();
    let payload = base64::engine::general_purpose::STANDARD
        .encode(marshal::marshal(&offer_stanza(&caller, now))?);
    r.call_embind(
        "handleIncomingSignalingOffer",
        &[
            Value::Str(payload),
            Value::Str("web".into()),
            Value::Str("2.3000.0".into()),
            Value::Str(now.to_string()),
            Value::Str(now.to_string()),
            Value::Bool(false),
            Value::Bool(true),
            Value::Str(caller.to_string()),
            Value::Bytes(Vec::new()),
        ],
    )
    .ok();
    r.refuel();
    r.settle(std::time::Duration::from_secs(3));
    // Clear the incoming call, or the origination fails on an initialised call
    // context for a reason that has nothing to do with the arm being tested.
    r.call_embind("rejectCall", &[]).ok();
    r.refuel();
    r.settle(std::time::Duration::from_secs(3));
    Ok(())
}

/// The buffer the engine's SCTP data path writes through.
fn init_sctp(r: &mut Runtime) -> String {
    const SIZE: u32 = 1 << 20;
    let Ok(ptr) = r.malloc(SIZE) else {
        return "malloc failed".into();
    };
    let set_up = r.call_embind(
        "initSctpRingBuffer",
        &[Value::Int(i64::from(ptr)), Value::Int(i64::from(SIZE))],
    );
    r.refuel();
    let now = r.call_embind("isSctpRingBufferInitialized", &[]);
    r.refuel();
    format!("init={set_up:?} initialized={now:?}")
}

/// `JsWorkerThread.js`'s wrapper: start the thread, then read its pthread id.
fn start_worker(r: &mut Runtime) -> String {
    let worker = r.call_embind("startJsWorkerThread", &[]);
    r.refuel();
    r.settle(std::time::Duration::from_secs(2));
    let mut out = format!("start={worker:?}");
    if let Ok(handle) = &worker
        && let Some(id) = handle.as_int()
    {
        let pthread = r.call_embind("getJsWorkerPThreadId", &[Value::Int(id)]);
        r.refuel();
        out.push_str(&format!(" pthread={pthread:?}"));
    }
    out
}

fn main() -> anyhow::Result<()> {
    let which = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "S_ivh1PriOA".into());
    let catalog = Catalog::discover()?;
    let entry = catalog.resolve(&which)?;
    let bytes = std::fs::read(&entry.path)?;
    println!(
        "{}\n",
        entry.path.file_name().unwrap_or_default().to_string_lossy()
    );

    for arm in ARMS {
        println!("=== {}", arm.label);
        let mut r = engine(&bytes)?;

        if arm.ab_props {
            println!(
                "  ab props     {} of {}",
                set_ab_props(&mut r),
                AB_PROPS.len()
            );
        }
        if arm.settings {
            load_settings(&mut r)?;
            println!("  settings     loaded");
        }
        if arm.sctp {
            println!("  sctp ring    {}", init_sctp(&mut r));
        }
        if arm.worker {
            println!("  js worker    {}", start_worker(&mut r));
        }

        let mark = r.engine_log().len();
        // Empty the recorded-call list so the origination's own host calls fit
        // inside MAX_TRACE and their arguments survive. Startup alone makes
        // tens of millions, which is why the list is useless without this.
        r.shared().clear_trace();
        let result = r.call_embind(
            "startVoipCall",
            &[
                Value::Str(PEER_LID.into()),
                Value::StringList(vec![PEER_LID_DEVICE.into()]),
                Value::Str(OUTGOING_CALL_ID.into()),
                Value::Bool(false),
                Value::Str(PEER_LID.into()),
                Value::Bool(false),
                Value::Bytes(vec![0xA5; 32]),
            ],
        );
        r.refuel();
        r.settle(std::time::Duration::from_secs(8));

        // Read the *counters*, not the recorded-call list. `all_calls_to` stops
        // growing at MAX_TRACE (8192) and this run makes tens of millions of
        // host calls, so a zero from it means "not in the first 8192" and
        // nothing more. Every earlier `sent=0` in this investigation was that
        // artefact.
        let counts = r.shared().hot_calls();
        let count = |symbol: &str| -> u64 {
            counts
                .iter()
                .find(|(s, _)| s == symbol)
                .map(|(_, n)| *n)
                .unwrap_or(0)
        };
        let sent = count("env::sendSignalingXMPP_js_sync");
        let sendto = count("env::call_sendto");
        let events = count("env::on_call_event_js_sync");
        let markers: Vec<i32> = r.shared().markers().iter().map(|(id, _)| *id).collect();
        if !markers.is_empty() {
            println!("  markers      {markers:?}");
        }

        // What the engine actually handed the host. The four words are the
        // block #855 unpacks: three heap pointers and a value. Reading them
        // back is the difference between "a call happened" and "here is the
        // stanza".
        for call in r.all_calls_to("env::sendSignalingXMPP_js_sync") {
            println!("  sendSignalingXMPP_js_sync{:?}", call.args);
            for (i, word) in call.args.iter().enumerate() {
                let addr = *word as u32;
                if addr == 0 || addr as usize >= r.memory_size() {
                    continue;
                }
                // Read it as bytes, and — when that looks like nothing — follow
                // it one level. libc++'s `std::string` is `{ptr, size, cap}`
                // when long, so a pointer to one dereferences to the text.
                let Ok(bytes) = r.read(addr, 256) else {
                    continue;
                };
                let direct = printable(&bytes);
                if direct.chars().filter(|c| *c != '.').count() > 4 {
                    println!("    arg{i} @{addr:#x}: {direct}");
                    continue;
                }
                let inner = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                let len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                if inner == 0 || inner as usize >= r.memory_size() || len > 1 << 20 {
                    // Not a `{ptr, size}` header. Both of these carry text at
                    // +8, so show the block as text and let the shape speak.
                    println!("    arg{i} @{addr:#x}: {}", loose(&bytes[..160]));
                    continue;
                }
                if let Ok(target) = r.read(inner, len.clamp(1, 400)) {
                    println!(
                        "    arg{i} @{addr:#x} -> {inner:#x} len={len}: {}",
                        printable(&target)
                    );
                }
            }
        }
        let lines = r.engine_log_from(mark);
        println!(
            "  startVoipCall -> {}   sent={sent} sendto={sendto} events={events} lines={}",
            match &result {
                Ok(v) => format!("{v:?}"),
                Err(_) => "trap".into(),
            },
            lines.len()
        );
        for line in lines
            .iter()
            .filter(|l| l.contains("offer") || l.contains("Offer") || l.contains("transport"))
        {
            println!("    {}", line.trim());
        }
        println!();
    }

    Ok(())
}
