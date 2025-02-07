use io::{Read, Seek};
use quadio_core as core;
use std::collections::HashMap;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;
use std::{env, fs, io};

const ARGUMENTS: [&str; 5] = ["in", "out", "start", "end", "duration"];
const INPUT_BUFFER_SZ: usize = 4096;

type CommandArgs = HashMap<&'static str, String>;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum CommandKind {
    Info,
    Play,
    PlayLooped,
    Strip,
    SetLoop,
    Blend,
    Help,
}

impl TryFrom<&str> for CommandKind {
    type Error = String;

    fn try_from(from: &str) -> Result<CommandKind, Self::Error> {
        match from {
            "info" => Ok(CommandKind::Info),
            "play" => Ok(CommandKind::Play),
            "loop" => Ok(CommandKind::PlayLooped),
            "set-loop" => Ok(CommandKind::SetLoop),
            "strip" => Ok(CommandKind::Strip),
            "blend" => Ok(CommandKind::Blend),
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
            } else if !map.contains_key("in") {
                map.insert("in", arg.into());
            } else {
                map.insert("out", arg.into());
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
        println!("QUADIO - Quake Looped Audio Utilities\n");
        usage();
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

                        let loop_end = info.end.unwrap_or(info.sample_count);

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
            CommandKind::Strip | CommandKind::SetLoop | CommandKind::Blend => {
                let q_wave_reader = core::QWaveReader::new(reader)?;
                let project = core::Project::from_reader(q_wave_reader)?;
                run_write_command((cmd, args), project)?;
            }
            CommandKind::Help => {
                unreachable!();
            }
        }
    }

    Ok(())
}

fn run_write_command(
    (cmd, args): Command,
    mut proj: core::Project,
) -> Result<(), String> {
    let outpath = Path::new(expect_arg(&args, "out")?);

    match cmd {
        CommandKind::Strip => {
            proj.set_loop(None);
        }
        CommandKind::SetLoop => {
            let start = parse_time(expect_arg(&args, "start")?, &proj)?;

            let end = args
                .get("end")
                .map(|e| parse_time(e, &proj))
                .transpose()?
                .unwrap_or(proj.samples().len().try_into().unwrap());

            proj.set_loop(Some(start..end));
        }
        CommandKind::Blend => {
            let blend_duration = args
                .get("duration")
                .map(|e| parse_time(e, &proj))
                .transpose()?;

            if let Some(window_sz) = blend_duration {
                proj.blend(window_sz)?;
            } else {
                proj.blend_default_window()?;
            }
        }
        _ => {
            unreachable!();
        }
    };

    proj.write_to(&outpath)?;

    Ok(())
}

fn main() {
    let args_owned: Vec<String> = env::args().skip(1).collect();
    let args = args_owned.iter().map(|arg| &arg[..]);

    let result = parse_args(args);

    if let Err(e) = result.and_then(run_command) {
        eprintln!("{}", e);

        if e.contains("sub-command") {
            usage();
        }
    }
}

fn parse_time(
    time_str: impl AsRef<str>,
    proj: &core::Project,
) -> Result<u32, String> {
    let time_str = time_str.as_ref();

    Ok(if time_str == "LAST" {
        proj.samples().len().try_into().unwrap()
    } else if let Some(stripped) = time_str.strip_suffix("ms") {
        let millis = stripped
            .parse::<f64>()
            .or(Err("Failed to parse time in milliseconds"))?;
        (millis / 1000.0 * f64::from(proj.sample_rate())).round() as u32
    } else if let Some(stripped) = time_str.strip_suffix("s") {
        let seconds = stripped
            .parse::<f64>()
            .or(Err("Failed to parse time in seconds"))?;
        (seconds * f64::from(proj.sample_rate())).round() as u32
    } else {
        time_str.parse::<u32>().or(Err("Failed to parse time"))?
    })
}

fn play_wave<R: Read + Seek>(reader: R, looped: bool) -> Result<(), String> {
    let key_reader = KeyReader::new().ok_or("Error creating key reader")?;
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

        if let Some(key) = key_reader.read() {
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

fn usage() {
    println!(
        r#"Usage: quadio-cli <sub-command> [<arg>...] [--] <input> [<output>]

Sub-commands:
    help
        Print usage

    info <input>
        Print information about WAV file

    play <input>
        Play file from start to end, ignoring loops

    loop <input>
        Play file with loops.  If file contains no loops, loop from file start
        to end

    set-loop -start=<TIME> [-end=<TIME>] [--] <input> <output>
        Set loop point, ranging from start to end.  If end is not provided,
        the last sample in the file is chosen.  Points in time are 0-based (0
        refers to the first sample)

    strip <input> <output>
        Strips loop (CUE and length markers) from file

    blend [-duration=<TIME>] [--] <input> <output>
        Blends samples from a *duration* window before the loop starts with
        samples a *duration* window before the loop ends.  Loop must start after
        *duration* and be at least as long as *duration*.  If the duration is
        not provided, the smallest value is chosen which should eliminate
        clicks and pops in playback

Time:
    Time arguments (start, end, duration) are given in non-zero integer numbers
    of samples.  A suffix can be provided to use rational-valued times in the
    desired unit, seconds or milliseconds, e.g. '0.5s' for seconds or '111.1ms'
    for milliseconds.

Playback controls:
    space - Pause and resume playback.  Prints current sample on pause
    q     - Stop & quit
"#
    );
}

#[cfg(not(target_os = "windows"))]
struct KeyReader {
    old_attr: libc::termios,
}

#[cfg(not(target_os = "windows"))]
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
        let mut buffer = vec![0u8; INPUT_BUFFER_SZ];

        let ret = unsafe {
            libc::read(
                libc::STDIN_FILENO,
                buffer.as_mut_ptr() as *mut _,
                INPUT_BUFFER_SZ,
            )
        };

        if ret > 0 {
            Some(buffer[ret as usize - 1])
        } else {
            None
        }
    }
}

#[cfg(not(target_os = "windows"))]
impl Drop for KeyReader {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.old_attr);
        }
    }
}

#[cfg(target_os = "windows")]
use winapi::um::{
    consoleapi as conapi, processenv, winbase as base, wincon as con,
};

#[cfg(target_os = "windows")]
struct KeyReader {
    old_mode: u32,
}

#[cfg(target_os = "windows")]
impl KeyReader {
    pub fn new() -> Option<Self> {
        let mut con_mode = 0u32;

        let h_stdin =
            unsafe { processenv::GetStdHandle(base::STD_INPUT_HANDLE) };

        unsafe {
            if conapi::GetConsoleMode(h_stdin, &mut con_mode) == 0 {
                return None;
            }
        }

        let old_mode = con_mode;
        con_mode &= !(con::ENABLE_ECHO_INPUT | con::ENABLE_LINE_INPUT);

        unsafe {
            if conapi::SetConsoleMode(h_stdin, con_mode) == 0 {
                return None;
            }
        }

        Some(Self { old_mode })
    }

    pub fn read(&self) -> Option<u8> {
        let mut peek_buffer: [con::INPUT_RECORD; 1] =
            unsafe { std::mem::zeroed() };
        let mut peeked_records = 0u32;

        let h_stdin =
            unsafe { processenv::GetStdHandle(base::STD_INPUT_HANDLE) };

        unsafe {
            if conapi::PeekConsoleInputA(
                h_stdin,
                &mut peek_buffer[0],
                1,
                &mut peeked_records,
            ) == 0
            {
                return None;
            }
        }

        if peeked_records == 0 {
            return None;
        }

        let mut read_buffer: [con::INPUT_RECORD; INPUT_BUFFER_SZ] =
            unsafe { std::mem::zeroed() };
        let mut read_records = 0u32;

        unsafe {
            if conapi::ReadConsoleInputA(
                h_stdin,
                &mut read_buffer[0],
                INPUT_BUFFER_SZ as u32,
                &mut read_records,
            ) == 0
            {
                return None;
            }
        }

        let mut indices = (0..read_records as usize).collect::<Vec<_>>();
        indices.reverse();

        for i in indices {
            if read_buffer[i].EventType == con::KEY_EVENT {
                let evt = unsafe { read_buffer[i].Event.KeyEvent() };

                if evt.bKeyDown != 0 {
                    return Some(*unsafe { evt.uChar.AsciiChar() } as u8);
                }
            }
        }

        None
    }
}

#[cfg(target_os = "windows")]
impl Drop for KeyReader {
    fn drop(&mut self) {
        unsafe {
            let h_stdin = processenv::GetStdHandle(base::STD_INPUT_HANDLE);
            conapi::SetConsoleMode(h_stdin, self.old_mode);
        }
    }
}
