use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use zhashdiff::{comparison::PairState, fs::FsNode};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tolerance {
    pub enabled: bool,
    pub size: bool,
    pub max_size_delta: SizeDelta,
    pub same_size_and_time: bool,
}

impl Default for Tolerance {
    fn default() -> Self {
        Self {
            enabled: false,
            size: true,
            max_size_delta: SizeDelta::Percent(1.0),
            same_size_and_time: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SizeDelta {
    Bytes(u64),
    /// Of the larger size.
    Percent(f64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SideMeta {
    pub size: u64,
    pub modified: Option<SystemTime>,
}

impl SideMeta {
    pub fn of(node: &FsNode) -> Self {
        Self {
            size: node.size,
            modified: node.modified,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Passed {
    SizeDelta { sizes: [u64; 2] },
    SameSizeAndTime { size: u64 },
}

impl Tolerance {
    /// `exact` is None before the pair was compared. Then only the modification-time criterion
    /// can decide, so the pair is never read.
    pub fn is_similar(&self, sides: [Option<SideMeta>; 2], exact: Option<PairState>) -> bool {
        let [Some(left), Some(right)] = sides else {
            return false;
        };
        let decides = match exact {
            Some(PairState::Same) => false,
            Some(PairState::Different) => self.size || self.same_size_and_time,
            None => self.same_size_and_time,
        };
        self.enabled
            && decides
            && (!self.size || self.size_passes([left, right]))
            && (!self.same_size_and_time || same_size_and_time([left, right]))
    }

    pub fn passed(&self, sides: [SideMeta; 2]) -> Vec<Passed> {
        let mut passed = Vec::new();
        if self.size && self.size_passes(sides) {
            passed.push(Passed::SizeDelta {
                sizes: sides.map(|side| side.size),
            });
        }
        if self.same_size_and_time && same_size_and_time(sides) {
            passed.push(Passed::SameSizeAndTime {
                size: sides[0].size,
            });
        }
        passed
    }

    fn size_passes(&self, [left, right]: [SideMeta; 2]) -> bool {
        let delta = left.size.abs_diff(right.size);
        match self.max_size_delta {
            SizeDelta::Bytes(max) => delta <= max,
            SizeDelta::Percent(max) => {
                delta as f64 * 100.0 <= max * left.size.max(right.size) as f64
            }
        }
    }
}

fn same_size_and_time([left, right]: [SideMeta; 2]) -> bool {
    left.size == right.size && left.modified.is_some() && left.modified == right.modified
}

/// `sides` is None for a folder.
pub fn similar_tooltip(tolerance: &Tolerance, sides: Option<[SideMeta; 2]>) -> String {
    let Some(sides) = sides else {
        return "Similar: every file below is the same or similar".to_owned();
    };
    let mut text = "Similar:".to_owned();
    for passed in tolerance.passed(sides) {
        let line = match passed {
            Passed::SizeDelta {
                sizes: [left, right],
            } => {
                let delta = left.abs_diff(right);
                let larger = left.max(right);
                let percent = if larger == 0 {
                    0.0
                } else {
                    delta as f64 * 100.0 / larger as f64
                };
                let max = match tolerance.max_size_delta {
                    SizeDelta::Bytes(max) => format!("{max} bytes"),
                    SizeDelta::Percent(max) => format!("{max} %"),
                };
                format!(
                    "sizes {left} and {right} bytes differ by {delta} bytes ({percent:.2} %), at most {max}"
                )
            }
            Passed::SameSizeAndTime { size } => {
                format!("same size ({size} bytes) and modification time")
            }
        };
        text.push('\n');
        text.push_str(&line);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{Passed, SideMeta, SizeDelta, Tolerance, similar_tooltip};
    use std::time::{Duration, SystemTime};
    use zhashdiff::comparison::PairState;

    const DIFFERENT: Option<PairState> = Some(PairState::Different);

    fn time(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn sides(sizes: [u64; 2], times: [u64; 2]) -> [Option<SideMeta>; 2] {
        [0, 1].map(|i| {
            Some(SideMeta {
                size: sizes[i],
                modified: Some(time(times[i])),
            })
        })
    }

    fn size_only(max_size_delta: SizeDelta) -> Tolerance {
        Tolerance {
            enabled: true,
            size: true,
            max_size_delta,
            same_size_and_time: false,
        }
    }

    fn time_only() -> Tolerance {
        Tolerance {
            enabled: true,
            size: false,
            max_size_delta: SizeDelta::Bytes(0),
            same_size_and_time: true,
        }
    }

    #[test]
    fn the_size_criterion_in_bytes_passes_up_to_its_threshold() {
        let tolerance = size_only(SizeDelta::Bytes(10));
        assert!(tolerance.is_similar(sides([100, 110], [1, 2]), DIFFERENT));
        assert!(tolerance.is_similar(sides([110, 100], [1, 2]), DIFFERENT));
        assert!(!tolerance.is_similar(sides([100, 111], [1, 2]), DIFFERENT));
        assert!(!tolerance.is_similar(sides([111, 100], [1, 2]), DIFFERENT));
    }

    #[test]
    fn the_size_criterion_in_percent_passes_up_to_its_share_of_the_larger_size() {
        let tolerance = size_only(SizeDelta::Percent(1.0));
        assert!(tolerance.is_similar(sides([1000, 1010], [1, 2]), DIFFERENT));
        assert!(tolerance.is_similar(sides([1010, 1000], [1, 2]), DIFFERENT));
        assert!(!tolerance.is_similar(sides([1000, 1011], [1, 2]), DIFFERENT));
        assert!(tolerance.is_similar(sides([0, 0], [1, 2]), DIFFERENT));
        assert!(!tolerance.is_similar(sides([0, 1], [1, 2]), DIFFERENT));
    }

    #[test]
    fn the_time_criterion_needs_the_same_size_and_modification_time() {
        let tolerance = time_only();
        assert!(tolerance.is_similar(sides([5, 5], [7, 7]), DIFFERENT));
        assert!(!tolerance.is_similar(sides([5, 5], [7, 8]), DIFFERENT));
        assert!(!tolerance.is_similar(sides([5, 6], [7, 7]), DIFFERENT));
        let no_time = SideMeta {
            size: 5,
            modified: None,
        };
        assert!(!tolerance.is_similar([Some(no_time), Some(no_time)], DIFFERENT));
    }

    #[test]
    fn a_pair_failing_one_of_two_criteria_stays_different() {
        let tolerance = Tolerance {
            same_size_and_time: true,
            ..size_only(SizeDelta::Bytes(10))
        };
        assert!(!tolerance.is_similar(sides([100, 105], [7, 7]), DIFFERENT));
        assert!(!tolerance.is_similar(sides([100, 100], [7, 8]), DIFFERENT));
        assert!(tolerance.is_similar(sides([100, 100], [7, 7]), DIFFERENT));
    }

    #[test]
    fn one_sided_pairs_are_never_similar() {
        let tolerance = Tolerance {
            same_size_and_time: true,
            ..size_only(SizeDelta::Bytes(10))
        };
        let [left, right] = sides([5, 5], [7, 7]);
        assert!(!tolerance.is_similar([left, None], DIFFERENT));
        assert!(!tolerance.is_similar([None, right], DIFFERENT));
        assert!(!tolerance.is_similar([left, None], None));
    }

    #[test]
    fn only_the_time_criterion_decides_a_pair_that_was_not_compared() {
        assert!(time_only().is_similar(sides([5, 5], [7, 7]), None));
        assert!(!time_only().is_similar(sides([5, 5], [7, 8]), None));
        assert!(!size_only(SizeDelta::Bytes(10)).is_similar(sides([5, 6], [7, 7]), None));
        let both = Tolerance {
            same_size_and_time: true,
            ..size_only(SizeDelta::Bytes(10))
        };
        assert!(both.is_similar(sides([5, 5], [7, 7]), None));
    }

    #[test]
    fn same_pairs_stay_same_and_nothing_passes_while_off_or_without_criteria() {
        let tolerance = time_only();
        assert!(!tolerance.is_similar(sides([5, 5], [7, 7]), Some(PairState::Same)));
        let off = Tolerance {
            enabled: false,
            ..tolerance
        };
        assert!(!off.is_similar(sides([5, 5], [7, 7]), DIFFERENT));
        let no_criteria = Tolerance {
            same_size_and_time: false,
            ..tolerance
        };
        assert!(!no_criteria.is_similar(sides([5, 5], [7, 7]), DIFFERENT));
    }

    #[test]
    fn passed_lists_the_enabled_criteria_that_pass_with_their_values() {
        let both = Tolerance {
            same_size_and_time: true,
            ..size_only(SizeDelta::Bytes(10))
        };
        let [Some(left), Some(right)] = sides([5, 5], [7, 7]) else {
            unreachable!()
        };
        assert_eq!(
            both.passed([left, right]),
            [
                Passed::SizeDelta { sizes: [5, 5] },
                Passed::SameSizeAndTime { size: 5 }
            ]
        );
        let [Some(left), Some(right)] = sides([5, 8], [7, 7]) else {
            unreachable!()
        };
        assert_eq!(
            both.passed([left, right]),
            [Passed::SizeDelta { sizes: [5, 8] }]
        );
        assert_eq!(time_only().passed([left, right]), []);
    }

    #[test]
    fn the_similar_tooltip_lists_the_passed_criteria_with_their_values() {
        let percent = Tolerance {
            same_size_and_time: true,
            ..size_only(SizeDelta::Percent(1.0))
        };
        let [Some(left), Some(right)] = sides([1000, 1000], [7, 7]) else {
            unreachable!()
        };
        assert_eq!(
            similar_tooltip(&percent, Some([left, right])),
            "Similar:\n\
             sizes 1000 and 1000 bytes differ by 0 bytes (0.00 %), at most 1 %\n\
             same size (1000 bytes) and modification time"
        );
        let [Some(left), Some(right)] = sides([100, 108], [7, 8]) else {
            unreachable!()
        };
        assert_eq!(
            similar_tooltip(&size_only(SizeDelta::Bytes(10)), Some([left, right])),
            "Similar:\nsizes 100 and 108 bytes differ by 8 bytes (7.41 %), at most 10 bytes"
        );
        assert_eq!(
            similar_tooltip(&percent, None),
            "Similar: every file below is the same or similar"
        );
    }
}
