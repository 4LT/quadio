use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat, SampleRate};
use io::{Read, Seek};
use quadio_core as core;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

const ARGUMENTS: [&str; 2] = ["in", "out"];

type CommandArgs = HashMap<&'static str, String>;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum CommandKind {
    Info,
    Play,
    Help,
}

impl TryFrom<&str> for CommandKind {
    type Error = String;

    fn try_from(from: &str) -> Result<CommandKind, Self::Error> {
        match from {
            "info" => Ok(CommandKind::Info),
            "play" => Ok(CommandKind::Play),
            "help" => Ok(CommandKind::Help),
            other => Err(format!("Unknown sub-command \"{}\"", other)),
        }
    }
}

type Command = (CommandKind, CommandArgs);

fn parse_arg_param(arg_param: &str) -> Result<(&'static str, String), String> {
    let mut arg_param_iter = arg_param.splitn(2, '=');
    let arg_slice = arg_param_iter.next().unwrap();
    let param = arg_param_iter.next().map(String::from).unwrap_or("".into());

    let arg = ARGUMENTS.into_iter().find(|&s| s == arg_slice);

    match arg {
        None => Err(format!("Unrecognized argument {}", arg_slice)),
        Some(a) => Ok((a, param)),
    }
}

fn parse_args<'a, T: Iterator<Item = &'a str>>(
    mut args: T,
) -> Result<Command, String> {
    let cmd = args
        .next()
        .map(|cmd| cmd.try_into())
        .ok_or(String::from("Missing sub-command"))
        .and_then(|x| x)?;

    let mut map = HashMap::new();
    let mut reached_end = false;
    let mut reached_divider = false;

    while !reached_end {
        if let Some(arg) = args.next() {
            if arg.starts_with('-') && !reached_divider {
                if arg == "--" {
                    reached_divider = true;
                } else {
                    let arg = arg.trim_start_matches('-');
                    let (argname, param) = parse_arg_param(arg)?;
                    map.insert(argname, param);
                }
            } else {
                map.insert("in", arg.into());
                reached_end = true;
            }
        } else {
            reached_end = true;
        }
    }

    if let (Some(last), true) = (args.next(), reached_end) {
        return Err(format!("Unrecognized argument \"{}\"", last));
    }

    Ok((cmd, map))
}

fn expect_arg<'a>(
    args: &'a CommandArgs,
    argname: &str,
) -> Result<&'a String, String> {
    args.get(argname).ok_or_else(|| {
        if argname == "in" {
            "No input file provided".into()
        } else {
            format!("Expected argument \"{}\"", argname)
        }
    })
}

fn run_command((cmd, args): Command) -> Result<(), String> {
    if cmd == CommandKind::Help {
        eprintln!("Help!?")
    } else {
        let inpath = Path::new(expect_arg(&args, "in")?);
        let file = fs::File::open(inpath).map_err(|e| e.to_string())?;
        let reader = io::BufReader::new(file);

        match cmd {
            CommandKind::Info => {
                let info = core::QWaveReader::new(reader)?.metadata();
                println!("Information");
                println!("\tChannels = {}", info.spec.channels);
                println!("\tSample rate = {}", info.spec.sample_rate);
                println!("\tSample bits = {}", info.spec.bits_per_sample);

                let duration_s =
                    f64::from(info.duration) / f64::from(info.spec.sample_rate);

                println!(
                    "\tDuration = {} samples ({:.3}s)",
                    info.duration, duration_s,
                );

                match info.cue {
                    Some(c) => {
                        let cue_time = f64::from(c.sample_offset)
                            / f64::from(info.spec.sample_rate);

                        println!(
                            "\tLoop at sample {} ({:.3}s)",
                            c.sample_offset, cue_time,
                        );
                    }
                    None => println!("No loop point found"),
                }
            }
            CommandKind::Play => {
                play_wave(reader)?;
            }
            CommandKind::Help => {}
        }
    }

    Ok(())
}

fn main() {
    let args_owned: Vec<String> = env::args().skip(1).collect();
    let args = args_owned.iter().map(|arg| &arg[..]);

    let result = parse_args(args);

    if let Err(e) = result.and_then(run_command) {
        eprintln!("{}", e);
    }
}

fn play_wave<R: Read + Seek>(reader: R) -> Result<(), String> {
    let host = cpal::default_host();

    let device = host
        .default_output_device()
        .ok_or("Output device not found")?;

    let mut wave_reader = core::QWaveReader::new(reader)?;
    let metadata = wave_reader.metadata();
    let loop_sample = metadata.cue.map(|c| c.sample_offset);

    if let Some(s) = loop_sample {
        if s > metadata.duration {
            return Err("Loop sample exceeds file duration".into());
        }
    }

    let desired_rate = metadata.spec.sample_rate;

    let duration = match metadata.cue {
        None => Some(Duration::from_millis(
            (metadata.duration * 1000 / desired_rate + 1).into(),
        )),
        Some(_) => None,
    };

    let config = device
        .supported_output_configs()
        .map_err(|e| e.to_string())?
        .filter(|cfg| {
            cfg.channels() == 1 && cfg.sample_format() == SampleFormat::F32
        })
        .map(|cfg| cfg.try_with_sample_rate(SampleRate(desired_rate)))
        .next()
        .ok_or("Could not find appropriate configuration")?
        .ok_or("Could not acquire stream with desired sample rate")?;

    let samples = wave_reader.collect_samples()?;

    println!("Playing. . .");

    let mut offset = 0usize;

    let paint_samples = move |buf: &mut [f32], _: &'_ _| {
        let samples_len = samples.len();
        let in_end = samples_len.min(offset + buf.len());
        let sample_ct = in_end.saturating_sub(offset);

        if let (Some(loop_start), true) = (loop_sample, in_end != 0) {
            let loop_start = loop_start as usize;
            let loop_len = samples_len - loop_start;

            let wrap = |off: usize| {
                if off > samples_len {
                    (off - loop_start) % loop_len + loop_start
                } else {
                    off
                }
            };

            let mut write_start = 0usize;
            let mut write_end;

            loop {
                let write_count = sample_ct
                    .min(buf.len() - write_start)
                    .min(samples_len - offset);

                write_end = write_start + write_count;
                let read_end = offset + write_count;

                buf[write_start..write_end]
                    .copy_from_slice(&samples[offset..read_end]);

                offset += sample_ct;
                offset = wrap(offset);
                write_start += write_count;

                if write_start >= buf.len() {
                    break;
                }
            }
        } else if offset < samples_len {
            let write_count =
                sample_ct.min(buf.len()).min(samples_len - offset);

            let read_end = offset + write_count;

            buf[..write_count].copy_from_slice(&samples[offset..read_end]);

            buf[write_count..].fill(f32::EQUILIBRIUM);
            offset += buf.len();
        } else {
            buf.fill(f32::EQUILIBRIUM);
        }
    };

    let stream = device
        .build_output_stream(
            &config.into(),
            paint_samples,
            move |_| {},
            duration,
        )
        .map_err(|e| e.to_string())?;

    stream.play().unwrap();

    if let Some(duration) = duration {
        sleep(duration);
    } else {
        loop {
            sleep(Duration::from_millis(10));
        }
    }

    println!("Done.");

    Ok(())
}
