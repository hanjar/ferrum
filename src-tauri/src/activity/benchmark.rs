pub const DAY_MS: i64 = 86_400_000;
pub const FIXED_NOW_UTC_MS: i64 = 100 * DAY_MS + 12 * 3_600_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntheticEntity {
    pub id: String,
    pub name: String,
    pub path: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntheticEpisode {
    pub entity_index: usize,
    pub last_utc_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fixture {
    pub entities: Vec<SyntheticEntity>,
    pub episodes: Vec<SyntheticEpisode>,
}

/// Synthetic IDs and paths never refer to user files. Same parameters yield same replay.
pub fn generate(entity_count: usize, episode_count: usize, seed: usize) -> Fixture {
    assert!(entity_count > 0);
    assert!(entity_count <= 1_000_000);
    assert!(episode_count <= 10_000_000);
    let entities = (0..entity_count)
        .map(|index| {
            let name = match index {
                0 => "project".to_owned(),
                1 => "proejct".to_owned(),
                2 => "Invoice 2026".to_owned(),
                _ => format!("item-{index:06}"),
            };
            SyntheticEntity {
                id: format!("entity-{index:06}"),
                path: format!("/fixture/root-{}/{}", index % 16, name),
                name,
            }
        })
        .collect();
    let episodes = (0..episode_count)
        .map(|index| {
            let entity_index = index.wrapping_mul(65_537).wrapping_add(seed) % entity_count;
            let age_days = (index / entity_count + entity_index * 13) % 90;
            SyntheticEpisode {
                entity_index,
                last_utc_ms: FIXED_NOW_UTC_MS
                    - age_days as i64 * DAY_MS
                    - (index % 1_000) as i64 * 1_000,
            }
        })
        .collect();
    Fixture { entities, episodes }
}
