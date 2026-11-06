//! Per-frame parameter expressions. Selected numeric settings may hold a
//! tiny math expression (see `NumOrExpr`) instead of a constant. Expressions
//! are parsed once when the settings change and evaluated exactly once per
//! frame against a small read-only context — never per particle — so a
//! shared theme can't spend meaningful CPU.
//!
//! Variables: `t` (monotonic seconds), `speed` (smoothed cursor px/s),
//! `moving` (1 while the cursor moves, else 0), `pi`.
//! Functions: `sin`, `cos`, `abs`, `floor`, `ceil`, `round`, `sqrt`,
//! `min(a, …)`, `max(a, …)` and `clamp(x, lo, hi)`. evalexpr ships no
//! built-in functions, so this set is registered on the frame context.

use evalexpr::{
    build_operator_tree, ContextWithMutableFunctions, ContextWithMutableVariables,
    DefaultNumericTypes, EvalexprError, Function, HashMapContext, Node, Value,
};

use crate::settings::NumOrExpr;
use crate::store::EffectSettings;

/// Expression length cap — keeps parse and eval cost bounded for shared
/// themes.
pub const MAX_EXPR_LEN: usize = 200;

/// The settings fields that may hold expressions.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Field {
    TrailWidth,
    TrailHueSpeed,
    BubbleRate,
    BubbleBuoyancy,
    BubbleWobble,
    BubbleSpin,
    SparkRate,
    SparkGravity,
}

impl Field {
    const ALL: [Field; 8] = [
        Field::TrailWidth,
        Field::TrailHueSpeed,
        Field::BubbleRate,
        Field::BubbleBuoyancy,
        Field::BubbleWobble,
        Field::BubbleSpin,
        Field::SparkRate,
        Field::SparkGravity,
    ];

    fn get(self, es: &EffectSettings) -> &NumOrExpr {
        match self {
            Field::TrailWidth => &es.trail.width,
            Field::TrailHueSpeed => &es.trail.hue_speed,
            Field::BubbleRate => &es.bubbles.rate,
            Field::BubbleBuoyancy => &es.bubbles.buoyancy,
            Field::BubbleWobble => &es.bubbles.wobble,
            Field::BubbleSpin => &es.bubbles.spin,
            Field::SparkRate => &es.sparkles.rate,
            Field::SparkGravity => &es.sparkles.gravity,
        }
    }

    fn set(self, es: &mut EffectSettings, v: f32) {
        match self {
            Field::TrailWidth => es.trail.width = NumOrExpr::Num(v),
            Field::TrailHueSpeed => es.trail.hue_speed = NumOrExpr::Num(v),
            Field::BubbleRate => es.bubbles.rate = NumOrExpr::Num(v),
            Field::BubbleBuoyancy => es.bubbles.buoyancy = NumOrExpr::Num(v),
            Field::BubbleWobble => es.bubbles.wobble = NumOrExpr::Num(v),
            Field::BubbleSpin => es.bubbles.spin = NumOrExpr::Num(v),
            Field::SparkRate => es.sparkles.rate = NumOrExpr::Num(v),
            Field::SparkGravity => es.sparkles.gravity = NumOrExpr::Num(v),
        }
    }

    /// Safe fallback value when the expression can't be parsed or evaluated:
    /// the field's own configured default, read from `EffectSettings`.
    fn default_value(self) -> f32 {
        let d = EffectSettings::default();
        match self {
            Field::TrailWidth => d.trail.width.value(),
            Field::TrailHueSpeed => d.trail.hue_speed.value(),
            Field::BubbleRate => d.bubbles.rate.value(),
            Field::BubbleBuoyancy => d.bubbles.buoyancy.value(),
            Field::BubbleWobble => d.bubbles.wobble.value(),
            Field::BubbleSpin => d.bubbles.spin.value(),
            Field::SparkRate => d.sparkles.rate.value(),
            Field::SparkGravity => d.sparkles.gravity.value(),
        }
    }

    /// Safety clamp so a shared theme can't produce pathological values
    /// (particle floods, screen-wide strokes) via an expression.
    fn clamp(self, v: f32) -> f32 {
        match self {
            Field::TrailWidth => v.clamp(0.2, 200.0),
            Field::TrailHueSpeed => v.clamp(0.0, 10.0),
            Field::BubbleRate => v.clamp(0.0, 1000.0),
            Field::BubbleBuoyancy => v.clamp(-2000.0, 2000.0),
            Field::BubbleWobble => v.clamp(0.0, 2000.0),
            Field::BubbleSpin => v.clamp(-50.0, 50.0),
            Field::SparkRate => v.clamp(0.0, 1000.0),
            Field::SparkGravity => v.clamp(-2000.0, 2000.0),
        }
    }
}

/// Single-argument math function.
fn fn1(f: impl Fn(f64) -> f64 + Clone + Send + Sync + 'static) -> Function<DefaultNumericTypes> {
    Function::new(move |arg| {
        let x: f64 = arg.as_number()?;
        Ok(Value::Float(f(x)))
    })
}

/// N-argument function (min/max over one or more values).
fn fnn(f: impl Fn(&[f64]) -> f64 + Clone + Send + Sync + 'static) -> Function<DefaultNumericTypes> {
    Function::new(move |arg| {
        let args = arg.as_tuple()?;
        let mut xs = Vec::with_capacity(args.len());
        for v in &args {
            xs.push(v.as_number()?);
        }
        if xs.is_empty() {
            return Err(EvalexprError::expected_number(arg.clone()));
        }
        Ok(Value::Float(f(&xs)))
    })
}

/// The expression function set, bound into a fresh frame context. evalexpr
/// ships no built-in functions, so everything documented for theme authors
/// is registered here.
fn frame_context(t: f64, speed: f32, moving: bool) -> HashMapContext<DefaultNumericTypes> {
    let mut ctx = HashMapContext::new();
    let _ = ctx.set_value("t".into(), Value::Float(t as f64));
    let _ = ctx.set_value("speed".into(), Value::Float(speed as f64));
    let _ = ctx.set_value("moving".into(), Value::Float(if moving { 1.0 } else { 0.0 }));
    let _ = ctx.set_value("pi".into(), Value::Float(std::f64::consts::PI));
    let _ = ctx.set_function("sin".into(), fn1(f64::sin));
    let _ = ctx.set_function("cos".into(), fn1(f64::cos));
    let _ = ctx.set_function("abs".into(), fn1(f64::abs));
    let _ = ctx.set_function("floor".into(), fn1(f64::floor));
    let _ = ctx.set_function("ceil".into(), fn1(f64::ceil));
    let _ = ctx.set_function("round".into(), fn1(f64::round));
    let _ = ctx.set_function("sqrt".into(), fn1(f64::sqrt));
    let _ = ctx.set_function("min".into(), fnn(|xs| xs.iter().cloned().fold(f64::INFINITY, f64::min)));
    let _ = ctx.set_function("max".into(), fnn(|xs| xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max)));
    let _ = ctx.set_function(
        "clamp".into(),
        Function::new(|arg| {
            let args = arg.as_tuple()?;
            if args.len() != 3 {
                return Err(EvalexprError::expected_number(arg.clone()));
            }
            let x: f64 = args[0].as_number()?;
            let lo: f64 = args[1].as_number()?;
            let hi: f64 = args[2].as_number()?;
            Ok(Value::Float(x.max(lo).min(hi)))
        }),
    );
    ctx
}

/// Parsed expression fields plus the base settings with those fields set to
/// their safe defaults. Built whenever the settings change; resolved once
/// per frame.
pub struct FrameExprs {
    base: EffectSettings,
    exprs: Vec<(Field, Node<DefaultNumericTypes>)>,
    /// Human-readable parse failures, surfaced in the settings UI.
    pub errors: Vec<String>,
}

impl FrameExprs {
    /// True when any field carries a parseable expression.
    #[allow(dead_code)] // exercised by tests; handy for diagnostics
    pub fn any(&self) -> bool {
        !self.exprs.is_empty()
    }

    /// Extract and parse every expression field. Constant-only settings
    /// produce an empty `exprs`, making `resolve` a plain clone.
    pub fn build(es: &EffectSettings) -> Self {
        let mut base = es.clone();
        let mut exprs = Vec::new();
        let mut errors = Vec::new();
        for field in Field::ALL {
            let value = field.get(&base).clone();
            if let NumOrExpr::Expr(src) = value {
                // The base carries the safe default; successful evaluations
                // overwrite it each frame.
                field.set(&mut base, field.default_value());
                if src.len() > MAX_EXPR_LEN {
                    errors.push(format!(
                        "expression too long ({} > {} characters)",
                        src.len(),
                        MAX_EXPR_LEN
                    ));
                    continue;
                }
                match build_operator_tree::<DefaultNumericTypes>(&src) {
                    Ok(node) => exprs.push((field, node)),
                    Err(e) => errors.push(format!("bad expression \u{201C}{}\u{201D}: {}", src, e)),
                }
            }
        }
        Self { base, exprs, errors }
    }

    /// Evaluate all expressions once for this frame and substitute the
    /// results. Constant-only configs short-circuit to a plain clone.
    /// `t` is f64: an always-on overlay accumulates days of uptime, and a
    /// f32 clock eventually loses sub-frame precision, making `sin(t)`
    /// step. The effect simulation keeps its own f32 `now` (particle ages
    /// stay small, so it never degrades).
    pub fn resolve(&self, t: f64, speed: f32, moving: bool) -> EffectSettings {
        if self.exprs.is_empty() {
            return self.base.clone();
        }
        let ctx = frame_context(t, speed, moving);
        let mut out = self.base.clone();
        for (field, node) in &self.exprs {
            let v = match node.eval_with_context(&ctx) {
                Ok(Value::Float(f)) => f as f32,
                Ok(Value::Int(i)) => i as f32,
                _ => field.default_value(),
            };
            // NaN survives f32::clamp ("returns NaN if self is NaN") and
            // would poison geometry, particle ages and dirty rects
            // downstream (sqrt of a negative, 0/0 …) — fall back to the
            // field default. ±inf is fine: clamp bounds it.
            let v = if v.is_nan() { field.default_value() } else { v };
            field.set(&mut out, field.clamp(v));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_buoyancy(value: NumOrExpr) -> EffectSettings {
        let mut es = EffectSettings::default();
        es.bubbles.buoyancy = value;
        es
    }

    #[test]
    fn constant_settings_take_the_fast_path() {
        let fe = FrameExprs::build(&EffectSettings::default());
        assert!(!fe.any());
        assert!(fe.errors.is_empty());
        // Resolve must equal the input verbatim.
        assert_eq!(fe.resolve(1.0, 100.0, true), EffectSettings::default());
    }

    #[test]
    fn expressions_evaluate_per_frame() {
        let fe = FrameExprs::build(&with_buoyancy(NumOrExpr::Expr("-20 + sin(t) * 10".into())));
        assert!(fe.any());
        let at_0 = fe.resolve(0.0, 0.0, false);
        let at_peak = fe.resolve(std::f32::consts::FRAC_PI_2 as f64, 0.0, false);
        assert!((at_0.bubbles.buoyancy.value() - (-20.0)).abs() < 0.01);
        assert!((at_peak.bubbles.buoyancy.value() - (-10.0)).abs() < 0.01);
    }

    #[test]
    fn frame_variables_flow_through() {
        let mut es = EffectSettings::default();
        es.trail.width = NumOrExpr::Expr("speed / 100 + moving * 2".into());
        let fe = FrameExprs::build(&es);
        assert_eq!(fe.resolve(0.0, 500.0, true).trail.width.value(), 7.0);
        assert_eq!(fe.resolve(0.0, 500.0, false).trail.width.value(), 5.0);
    }

    #[test]
    fn clamp_function_and_safety_clamps_work() {
        let mut es = EffectSettings::default();
        es.trail.width = NumOrExpr::Expr("clamp(999999, 2, 8)".into());
        let fe = FrameExprs::build(&es);
        assert_eq!(fe.resolve(0.0, 0.0, false).trail.width.value(), 8.0);

        // Even without an explicit clamp, the safety clamp bounds the value.
        es.trail.width = NumOrExpr::Expr("99999".into());
        let fe = FrameExprs::build(&es);
        assert_eq!(fe.resolve(0.0, 0.0, false).trail.width.value(), 200.0);
    }

    #[test]
    fn bad_expressions_fall_back_to_defaults_with_errors() {
        let mut es = EffectSettings::default();
        // Unmatched parenthesis: rejected at build time.
        es.trail.width = NumOrExpr::Expr("(1 + 2".into());
        let fe = FrameExprs::build(&es);
        assert!(!fe.errors.is_empty(), "parse failure is reported");
        // The field keeps its default, so rendering continues.
        assert_eq!(fe.resolve(0.0, 0.0, false).trail.width.value(), 10.0);

        // Unknown identifiers parse but fail at eval time — same fallback.
        es.trail.width = NumOrExpr::Expr("nope + 1".into());
        let fe = FrameExprs::build(&es);
        assert!(fe.errors.is_empty());
        assert_eq!(fe.resolve(0.0, 0.0, false).trail.width.value(), 10.0);

        // evalexpr tolerates dangling operators at build time; they fail at
        // eval and fall back the same way.
        es.trail.width = NumOrExpr::Expr("1 +".into());
        let fe = FrameExprs::build(&es);
        assert!(fe.errors.is_empty());
        assert_eq!(fe.resolve(0.0, 0.0, false).trail.width.value(), 10.0);
    }

    #[test]
    fn overlong_expressions_are_rejected() {
        let mut es = EffectSettings::default();
        es.trail.width = NumOrExpr::Expr("1 + 1 + ".repeat(60).into());
        let fe = FrameExprs::build(&es);
        assert!(!fe.errors.is_empty());
        assert!(fe.exprs.is_empty());
    }

    #[test]
    fn nan_expressions_fall_back_to_safe_values() {
        // sqrt of a negative parses fine and evaluates to NaN, which
        // f32::clamp would pass straight through into the effect geometry.
        let mut es = EffectSettings::default();
        es.bubbles.buoyancy = NumOrExpr::Expr("sqrt(t - 10)".into());
        let fe = FrameExprs::build(&es);
        let fallback = EffectSettings::default().bubbles.buoyancy.value();
        assert_eq!(fe.resolve(0.0, 0.0, false).bubbles.buoyancy.value(), fallback);
        // Once t passes 10 the expression is real again and takes over.
        let at = fe.resolve(14.0, 0.0, false).bubbles.buoyancy.value();
        assert!((at - 2.0).abs() < 0.01, "sqrt(4) = 2, got {}", at);
    }

    #[test]
    fn infinite_expressions_clamp_to_the_field_range() {
        let mut es = EffectSettings::default();
        es.bubbles.buoyancy = NumOrExpr::Expr("1e308 * 10".into()); // f64 inf
        let fe = FrameExprs::build(&es);
        assert_eq!(fe.resolve(0.0, 0.0, false).bubbles.buoyancy.value(), 2000.0);
    }

    #[test]
    fn num_or_expr_round_trips_json() {
        use crate::settings::TrailCfg;

        let cfg = TrailCfg {
            width: NumOrExpr::Expr("sin(t)".into()),
            hue_speed: NumOrExpr::Num(1.5),
            ..TrailCfg::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("\"width\":\"sin(t)\""), "expr serializes as string");
        assert!(json.contains("\"hue_speed\":1.5"), "number serializes as number");
        let back: TrailCfg = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);

        // A legacy config with plain numbers loads as constants.
        let legacy: TrailCfg =
            serde_json::from_str(r#"{"width": 12.0, "hue_speed": 2}"#).unwrap();
        assert_eq!(legacy.width, NumOrExpr::Num(12.0));
        assert_eq!(legacy.hue_speed, NumOrExpr::Num(2.0));
    }
}
