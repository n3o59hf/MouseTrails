use serde::{Deserialize, Serialize};

/// A numeric setting that is either a plain number or a tiny math expression
/// evaluated once per frame. Expressions use variables `t`, `speed`,
/// `moving`, `pi` and are documented for theme authors.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NumOrExpr {
    Num(f32),
    Expr(String),
}

impl NumOrExpr {
    /// Numeric value of a resolved field. Expressions never reach the effect
    /// code — they resolve to `Num` every frame before rendering.
    pub fn value(&self) -> f32 {
        match self {
            NumOrExpr::Num(n) => *n,
            NumOrExpr::Expr(_) => 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TrailStyle {
    #[default]
    Rainbow,
    Neon,
    Ghost,
    /// Bright core with two soft tinted halos (Soft = [core, edge1, edge2]).
    Soft,
    /// Solid colors cycling along the trail every `stripe_len` px.
    Striped,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrailCfg {
    pub enabled: bool,
    pub style: TrailStyle,
    pub length_secs: f32,
    /// Expression-capable per frame (variables t, speed, moving).
    pub width: NumOrExpr,
    /// Expression-capable per frame (variable t).
    pub hue_speed: NumOrExpr,
    pub color: [u8; 3],
    /// Style palette. Soft = [core, edge1, edge2]; Striped = cycling stripe
    /// colors. Empty falls back to per-style defaults.
    pub palette: Vec<[u8; 3]>,
    /// Striped style: length of one stripe segment in px along the trail.
    pub stripe_len: f32,
}

impl Default for TrailCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            style: TrailStyle::Rainbow,
            length_secs: 0.55,
            width: NumOrExpr::Num(10.0),
            hue_speed: NumOrExpr::Num(1.0),
            color: [51, 217, 255],
            palette: Vec::new(),
            stripe_len: 14.0,
        }
    }
}

/// What bubbles look like. Snowflakes skip particle/cursor collisions and
/// rotate; ornament balls spin from cursor speed and damp heavily.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BubbleShape {
    #[default]
    Bubble,
    Snowflake,
    Ornament,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BubbleCfg {
    pub enabled: bool,
    pub shape: BubbleShape,
    /// Expression-capable per frame (variables t, speed, moving).
    pub rate: NumOrExpr,
    pub size: f32,
    /// Expression-capable per frame (variable t). Negative values sink.
    pub buoyancy: NumOrExpr,
    pub lifetime: f32,
    /// Expression-capable per frame (variable t).
    pub wobble: NumOrExpr,
    /// Colors cycled per particle. Empty falls back to the built-in bubble
    /// hues.
    pub palette: Vec<[u8; 3]>,
    /// Fraction of cursor velocity inherited at spawn (0..1).
    pub momentum: f32,
    /// Exponential velocity decay per second — higher calms movement faster.
    pub drag: f32,
    /// Spin scale for rotatable shapes (snowflake: random direction at this
    /// rate; ornament: scales the speed-derived spin). Expression-capable
    /// per frame (variable t). rad/s.
    pub spin: NumOrExpr,
}

impl Default for BubbleCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            shape: BubbleShape::Bubble,
            rate: NumOrExpr::Num(9.0),
            size: 14.0,
            buoyancy: NumOrExpr::Num(85.0),
            lifetime: 2.4,
            wobble: NumOrExpr::Num(30.0),
            palette: Vec::new(),
            momentum: 0.35,
            drag: 2.2,
            spin: NumOrExpr::Num(0.0),
        }
    }
}

/// What sparkles look like. Stars are the classic four-point twinkle; dots
/// are soft round glows.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SparkShape {
    #[default]
    Star,
    Dot,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SparkleCfg {
    pub enabled: bool,
    pub shape: SparkShape,
    /// Expression-capable per frame (variables t, speed, moving).
    pub rate: NumOrExpr,
    pub size: f32,
    /// Expression-capable per frame (variable t).
    pub gravity: NumOrExpr,
    pub lifetime: f32,
    pub color: [u8; 3],
    /// Colors cycled per particle. Empty falls back to `color`.
    pub palette: Vec<[u8; 3]>,
    /// Fraction of cursor velocity inherited at spawn (0..1).
    pub momentum: f32,
    /// Exponential velocity decay per second.
    pub drag: f32,
    /// Motion streak length in seconds (0 = off): a fading line drawn from
    /// the particle back along its velocity.
    pub streak_secs: f32,
    /// Children spawned per second by each living spark (bengal-fire style).
    pub child_rate: f32,
    /// Size of child sparks relative to their parent.
    pub child_size: f32,
    /// Max generations: 1 = sparks never emit children, 3 = up to
    /// grandchildren.
    pub max_generations: u32,
}

impl Default for SparkleCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            shape: SparkShape::Star,
            rate: NumOrExpr::Num(40.0),
            size: 4.0,
            gravity: NumOrExpr::Num(-30.0),
            lifetime: 0.9,
            color: [255, 212, 64],
            palette: Vec::new(),
            momentum: 0.25,
            drag: 1.6,
            streak_secs: 0.0,
            child_rate: 0.0,
            child_size: 0.55,
            max_generations: 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RippleCfg {
    pub enabled: bool,
    pub rings: u32,
    pub speed: f32,
    pub lifetime: f32,
    pub width: f32,
    pub color: [u8; 3],
    /// Colors cycled per ring. Empty falls back to `color`.
    pub palette: Vec<[u8; 3]>,
    /// Angular wobble amplitude as a fraction of the radius (0 = perfect
    /// circles). Decays as a ring ages, settling into a clean circle.
    pub wobble: f32,
}

impl Default for RippleCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            rings: 2,
            speed: 450.0,
            lifetime: 0.6,
            width: 3.0,
            color: [140, 230, 255],
            palette: Vec::new(),
            wobble: 0.06,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trail_cfg_round_trips_palette() {
        let cfg = TrailCfg {
            palette: vec![[255, 255, 255], [255, 150, 200], [170, 200, 255]],
            stripe_len: 18.0,
            ..TrailCfg::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: TrailCfg = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
    }
}
