mod player;
pub use player::*;

mod reader;
pub use reader::*;

pub fn setup_player(
    wave_metadata: &Metadata,
    samples: &[i16],
) -> Result<Player, String> {
    let float_samples = samples
        .iter()
        .map(|&s| s as f32 / i16::MAX as f32)
        .collect::<Vec<_>>();

    let loop_start = wave_metadata
        .loop_start
        .and_then(|start| start.try_into().ok());

    let end = wave_metadata.end.and_then(|end| end.try_into().ok());

    let player_config = PlayerConfig {
        samples: float_samples,
        sample_rate: wave_metadata.sample_rate,
        loop_start,
        end,
    };

    Player::new(&player_config)
}
