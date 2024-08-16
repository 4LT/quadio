use io::{Read, Seek};
use quadio_core as core;
use std::collections::HashMap;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;
use std::{env, fs, io};

const ARGUMENTS: [&str; 2] = ["in", "out"];

type CommandArgs = HashMap<&'static str, String>;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum CommandKind {
    Info,
    Play,
    PlayLooped,
    Help,
}

impl TryFrom<&str> for CommandKind {
    type Error = String;

    fn try_from(from: &str) -> Result<CommandKind, Self::Error> {
        match from {
            "info" => Ok(CommandKind::Info),
            "play" => Ok(CommandKind::Play),
            "loop" => Ok(CommandKind::PlayLooped),
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
                println!("\tSample rate = {}", info.sample_rate);

                let duration_s =
                    f64::from(info.sample_count) / f64::from(info.sample_rate);

                println!(
                    "\tDuration = {} samples ({:.3}s)",
                    info.sample_count, duration_s,
                );

                match info.loop_start {
                    Some(start) => {
                        let cue_time =
                            f64::from(start) / f64::from(info.sample_rate);

                        println!(
                            "\tLoop starts at sample {} ({:.3}s)",
                            start, cue_time,
                        );

                        let loop_end =
                            info.end.or(Some(info.sample_count)).unwrap();

                        let end_time =
                            f64::from(loop_end) / f64::from(info.sample_rate);

                        println!(
                            "\tLoop ends at sample {} ({:.3}s)",
                            loop_end, end_time
                        );
                    }
                    None => println!("No loop point found"),
                }
            }
            CommandKind::Play => {
                play_wave(reader, false)?;
            }
            CommandKind::PlayLooped => {
                play_wave(reader, true)?;
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

fn play_wave<R: Read + Seek>(reader: R, looped: bool) -> Result<(), String> {
    let key_reader = KeyReader::new();

    if key_reader.is_none() {}

    let mut wave_reader = core::QWaveReader::new(reader)?;
    let mut quit = false;
    let mut done = false;
    let metadata = wave_reader.metadata();
    let samples = wave_reader.collect_samples()?;

    let mut player = core::setup_player(&metadata, &samples)?;
    player.play(0, looped)?;
    println!("Playing...");

    while !done {
        sleep(Duration::from_millis(30));

        if let Some(Some(key)) = key_reader.as_ref().map(|r| r.read()) {
            let state_tag = player.state();

            if key == b' ' {
                if state_tag == core::PlayerStateTag::Playing
                    || state_tag == core::PlayerStateTag::PlayingLooped
                {
                    player.pause();
                    let playhead_pos = player.playhead();
                    let playhead_time =
                        playhead_pos as f64 / f64::from(metadata.sample_rate);
                    println!(
                        "Paused at sample {} ({:.3}s)",
                        playhead_pos, playhead_time
                    );
                } else {
                    player.resume().unwrap();
                    println!("Resumed");
                }
            }

            if key == b'q' {
                quit = true;
                done = true;
            }
        }

        if player.samples_remaining() == 0 && !looped {
            done = true;
        }
    }

    if !quit {
        println!("Stopped.");
    }

    Ok(())
}

struct KeyReader {
    old_attr: libc::termios,
}

impl KeyReader {
    pub fn new() -> Option<Self> {
        let mut term_attr: libc::termios = unsafe { std::mem::zeroed() };

        unsafe {
            if libc::tcgetattr(libc::STDIN_FILENO, &mut term_attr) < 0 {
                return None;
            }
        }

        let old_attr = term_attr;
        term_attr.c_lflag &= !(libc::ECHO | libc::ICANON);
        term_attr.c_cc[libc::VMIN] = 0;
        term_attr.c_cc[libc::VTIME] = 0;

        unsafe {
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &term_attr)
                < 0
            {
                return None;
            }
        }

        Some(KeyReader { old_attr })
    }

    pub fn read(&self) -> Option<u8> {
        let mut buffer = vec![0u8; 4096];

        let ret = unsafe {
            libc::read(libc::STDIN_FILENO, buffer.as_mut_ptr() as *mut _, 4096)
        };

        if ret > 0 {
            Some(buffer[ret as usize - 1])
        } else {
            None
        }
    }
}

impl Drop for KeyReader {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.old_attr);
        }
    }
}
