use axiom_extension_abi::{
    encode, AbiVersion, CodecLimits, Frame, GuestMessage, HostMessage, Invocation,
    InvocationResult, RequestId, Value, ABI_VERSION,
};
use std::{env, fs, path::PathBuf, process::ExitCode};

const ITERATIONS: usize = 30;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("phase5k frame generator: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let output = env::args()
        .nth(1)
        .ok_or("usage: phase5k_frames <directory>")?;
    let output = PathBuf::from(output);
    fs::create_dir_all(&output)?;
    pair(
        &output,
        "negotiate",
        HostMessage::Negotiate {
            supported: vec![ABI_VERSION],
        },
        GuestMessage::Negotiated {
            selected: AbiVersion { major: 1, minor: 0 },
        },
    )?;
    pair(
        &output,
        "initialize",
        HostMessage::Initialize {
            instance_id: 1,
            application: "example.application".into(),
            extension: "example.phase5k".into(),
        },
        GuestMessage::Initialized,
    )?;
    let mut request = 1_u64;
    for bytes in [0_usize, 256, 1_024, 4_096] {
        for iteration in 0..ITERATIONS {
            let request_id = RequestId::new(request).map_err(|_| "invalid benchmark request ID")?;
            let value = Value::Bytes(vec![0x5a; bytes]);
            pair(
                &output,
                &format!("invoke-{bytes}-{iteration:02}"),
                HostMessage::Invoke {
                    request_id,
                    invocation: Invocation {
                        export: "echo".into(),
                        input: value.clone(),
                        snapshots: vec![],
                        deadline_unix_ms: u64::MAX,
                    },
                },
                GuestMessage::Completed {
                    request_id,
                    result: InvocationResult {
                        output: value,
                        patches: vec![],
                        transactions: vec![],
                        emitted_events: vec![],
                    },
                },
            )?;
            request += 1;
        }
    }
    pair(&output, "drain", HostMessage::Drain, GuestMessage::Drained)?;
    pair(
        &output,
        "shutdown",
        HostMessage::Shutdown,
        GuestMessage::Shutdown,
    )?;
    Ok(())
}

fn pair(
    output: &std::path::Path,
    name: &str,
    host: HostMessage,
    guest: GuestMessage,
) -> Result<(), Box<dyn std::error::Error>> {
    let limits = CodecLimits { max_bytes: 65_536 };
    fs::write(
        output.join(format!("{name}.in")),
        encode(&Frame::new(host), limits).map_err(|_| "could not encode host frame")?,
    )?;
    fs::write(
        output.join(format!("{name}.out")),
        encode(&Frame::new(guest), limits).map_err(|_| "could not encode guest frame")?,
    )?;
    Ok(())
}
